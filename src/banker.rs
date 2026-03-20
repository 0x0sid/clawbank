//! Credit line registry, deterministic scoring, force-recall, and reputation tracking.
//!
//! The Banker is the only component with **write access** to credit lines.
//! The Guardian has read-only access. This separation is a critical security invariant.

use crate::execution::treasury::TreasuryClient;
use crate::types::{
    Agent, AgentReputation, ApprovedConditions, CreditDecision, CreditLine, CreditProposal,
    CreditStatus, DashboardEvent,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};
use uuid::Uuid;

/// The Banker manages credit lines, scores proposals, and tracks agent reputation.
pub struct Banker {
    /// Active and historical credit lines, keyed by agent ID.
    credit_lines: Arc<RwLock<HashMap<Uuid, CreditLine>>>,
    /// Agent reputation records, keyed by agent ID.
    reputations: Arc<RwLock<HashMap<Uuid, AgentReputation>>>,
    /// Registered agents, keyed by agent ID.
    agents: Arc<RwLock<HashMap<Uuid, Agent>>>,
    /// Dashboard event broadcaster.
    tx: broadcast::Sender<DashboardEvent>,
    /// On-chain treasury client (None = stub mode, used in tests).
    treasury: Option<Arc<TreasuryClient>>,
}

impl Banker {
    /// Create a new Banker instance (no on-chain treasury — stub mode).
    /// Used in tests; production uses `with_treasury()`.
    #[allow(dead_code)]
    pub fn new(tx: broadcast::Sender<DashboardEvent>) -> Self {
        Self {
            credit_lines: Arc::new(RwLock::new(HashMap::new())),
            reputations: Arc::new(RwLock::new(HashMap::new())),
            agents: Arc::new(RwLock::new(HashMap::new())),
            tx,
            treasury: None,
        }
    }

    /// Create a Banker with an on-chain treasury client for production use.
    pub fn with_treasury(tx: broadcast::Sender<DashboardEvent>, treasury: Arc<TreasuryClient>) -> Self {
        Self {
            credit_lines: Arc::new(RwLock::new(HashMap::new())),
            reputations: Arc::new(RwLock::new(HashMap::new())),
            agents: Arc::new(RwLock::new(HashMap::new())),
            tx,
            treasury: Some(treasury),
        }
    }

    /// Register an agent. Returns the agent record. No trades possible without registration.
    pub async fn register_agent(&self, name: String) -> Agent {
        let agent = Agent {
            id: Uuid::new_v4(),
            name: name.clone(),
            registered_at: Utc::now(),
        };

        self.agents.write().await.insert(agent.id, agent.clone());

        // Initialize reputation for new agent
        let rep = AgentReputation { agent_id: agent.id, ..Default::default() };
        self.reputations.write().await.insert(agent.id, rep);

        info!(agent_id = %agent.id, name = %name, "Agent registered");

        let _ = self.tx.send(DashboardEvent::AgentRegistered {
            agent: agent.clone(),
        });

        agent
    }

    /// Check if an agent is registered.
    pub async fn is_registered(&self, agent_id: Uuid) -> bool {
        self.agents.read().await.contains_key(&agent_id)
    }

    /// Evaluate a credit proposal using deterministic scoring. Not AI — pure math.
    ///
    /// Scoring formula:
    /// ```text
    /// score = (
    ///   strategy_clarity  * 0.30
    ///   risk_return_ratio * 0.25
    ///   agent_reputation  * 0.30
    ///   collateral_quality* 0.15
    /// )
    /// score >= 6.0 -> approved (may reduce amount)
    /// score <  6.0 -> rejected with reason
    /// ```
    pub async fn evaluate(&self, proposal: &CreditProposal) -> CreditDecision {
        let reputation = self.reputation(proposal.agent_id).await;
        let score = self.compute_score(proposal, &reputation);

        info!(
            proposal_id = %proposal.id,
            agent_id = %proposal.agent_id,
            score = score,
            "Credit proposal scored"
        );

        if score < 6.0 {
            let reason = self.rejection_reason(proposal, score, &reputation);
            warn!(
                proposal_id = %proposal.id,
                score = score,
                reason = %reason,
                "Credit proposal rejected"
            );
            return CreditDecision {
                proposal_id: proposal.id,
                approved: false,
                approved_usd: None,
                rejection_reason: Some(reason),
                score,
                credit_line: None,
            };
        }

        // May reduce amount for borderline scores
        let approved_usd = if score < 7.0 {
            proposal.requested_usd * 0.75
        } else if score < 8.0 {
            proposal.requested_usd * 0.90
        } else {
            proposal.requested_usd
        };

        let credit_line = CreditLine {
            id: Uuid::new_v4(),
            proposal_id: proposal.id,
            agent_id: proposal.agent_id,
            approved_usd,
            spent_usd: 0.0,
            remaining_usd: approved_usd,
            status: CreditStatus::Active,
            approved_at: Utc::now(),
            expires_at: proposal.window_end,
            conditions: ApprovedConditions {
                allowed_pairs: proposal.allowed_pairs.clone(),
                max_single_trade_usd: proposal.max_single_trade_usd,
                max_loss_usd: proposal.max_loss_usd,
                window_end: proposal.window_end,
            },
            reputation_at_approval: reputation.score,
        };

        // Store the credit line
        self.credit_lines
            .write()
            .await
            .insert(proposal.agent_id, credit_line.clone());

        // Update reputation: lines_approved
        if let Some(rep) = self.reputations.write().await.get_mut(&proposal.agent_id) {
            rep.lines_approved += 1;
        }

        info!(
            credit_line_id = %credit_line.id,
            approved_usd = approved_usd,
            "Credit line granted"
        );

        // On-chain: call grantCredit on AgentTreasury contract
        if let Some(ref treasury) = self.treasury {
            // TODO(week3): Map agent UUID to EVM address via a registry.
            // For now, use the agent_id hex as a placeholder address.
            let agent_addr = format!("0x{}", proposal.agent_id.simple());
            if let Err(e) = treasury.grant_credit(&agent_addr, approved_usd, proposal.window_end).await {
                error!(agent_id = %proposal.agent_id, error = %e, "On-chain grantCredit failed");
            }
        }

        let _ = self.tx.send(DashboardEvent::CreditApproved {
            credit_line: credit_line.clone(),
        });

        CreditDecision {
            proposal_id: proposal.id,
            approved: true,
            approved_usd: Some(approved_usd),
            rejection_reason: None,
            score,
            credit_line: Some(credit_line),
        }
    }

    /// Get the active credit line for an agent, if any.
    /// Automatically marks expired lines as `Expired` so the poller skips them.
    pub async fn get_active_line(&self, agent_id: Uuid) -> Option<CreditLine> {
        let mut lines = self.credit_lines.write().await;
        let line = lines.get_mut(&agent_id)?;

        if line.status != CreditStatus::Active {
            return None;
        }

        if Utc::now() > line.expires_at {
            line.status = CreditStatus::Expired;
            info!(agent_id = %agent_id, "Credit line expired — status updated");
            return None;
        }

        Some(line.clone())
    }

    /// Deduct an amount from an agent's credit line after a trade is approved.
    pub async fn deduct(&self, agent_id: Uuid, amount: f64) -> Result<(), crate::types::AppError> {
        let mut lines = self.credit_lines.write().await;
        let line = lines
            .get_mut(&agent_id)
            .ok_or(crate::types::AppError::NoCreditLine(agent_id))?;

        if line.status != CreditStatus::Active {
            return Err(crate::types::AppError::NoCreditLine(agent_id));
        }

        if amount > line.remaining_usd {
            return Err(crate::types::AppError::InsufficientCredit {
                requested: amount,
                remaining: line.remaining_usd,
            });
        }

        line.spent_usd += amount;
        line.remaining_usd -= amount;

        info!(
            agent_id = %agent_id,
            amount = amount,
            remaining = line.remaining_usd,
            "Credit deducted"
        );

        let _ = self.tx.send(DashboardEvent::BudgetUpdate {
            agent_id,
            spent_usd: line.spent_usd,
            remaining_usd: line.remaining_usd,
        });

        Ok(())
    }

    /// Refund a previously deducted amount back to the credit line.
    /// Used when trade execution fails after deduction to prevent budget leaks.
    pub async fn refund(
        &self,
        agent_id: Uuid,
        amount: f64,
    ) -> Result<(), crate::types::AppError> {
        let mut lines = self.credit_lines.write().await;
        let line = lines
            .get_mut(&agent_id)
            .ok_or(crate::types::AppError::NoCreditLine(agent_id))?;

        line.spent_usd -= amount;
        line.remaining_usd += amount;

        info!(
            agent_id = %agent_id,
            amount = amount,
            remaining = line.remaining_usd,
            "Credit refunded after failed execution"
        );

        let _ = self.tx.send(DashboardEvent::BudgetUpdate {
            agent_id,
            spent_usd: line.spent_usd,
            remaining_usd: line.remaining_usd,
        });

        Ok(())
    }

    /// Force-recall a credit line. Blocks all future proposals until a new line is approved.
    pub async fn recall(
        &self,
        agent_id: Uuid,
        reason: String,
    ) -> Result<(), crate::types::AppError> {
        let mut lines = self.credit_lines.write().await;
        let line = lines
            .get_mut(&agent_id)
            .ok_or(crate::types::AppError::NoCreditLine(agent_id))?;

        line.status = CreditStatus::Recalled;

        // Update reputation negatively
        if let Some(rep) = self.reputations.write().await.get_mut(&agent_id) {
            rep.lines_recalled += 1;
            // Penalize score: each recall drops score by 1.5
            rep.score = (rep.score - 1.5).max(0.0);
        }

        warn!(
            agent_id = %agent_id,
            reason = %reason,
            "Credit line recalled"
        );

        // On-chain: call recallCredit on AgentTreasury contract
        if let Some(ref treasury) = self.treasury {
            let agent_addr = format!("0x{}", agent_id.simple());
            if let Err(e) = treasury.recall_credit(&agent_addr, &reason).await {
                error!(agent_id = %agent_id, error = %e, "On-chain recallCredit failed");
            }
        }

        let _ = self.tx.send(DashboardEvent::CreditRecalled {
            agent_id,
            reason,
        });

        Ok(())
    }

    /// Mark a credit line as repaid. Updates reputation positively.
    pub async fn repay(&self, agent_id: Uuid) -> Result<(), crate::types::AppError> {
        let mut lines = self.credit_lines.write().await;
        let line = lines
            .get_mut(&agent_id)
            .ok_or(crate::types::AppError::NoCreditLine(agent_id))?;

        let utilization = if line.approved_usd > 0.0 {
            line.spent_usd / line.approved_usd * 100.0
        } else {
            0.0
        };

        line.status = CreditStatus::Repaid;

        // Update reputation positively
        if let Some(rep) = self.reputations.write().await.get_mut(&agent_id) {
            rep.lines_repaid_cleanly += 1;
            // Reward: each clean repay adds 0.5
            rep.score = (rep.score + 0.5).min(10.0);
            // Rolling average utilization
            let total = rep.lines_repaid_cleanly as f64;
            rep.avg_utilization_pct =
                (rep.avg_utilization_pct * (total - 1.0) + utilization) / total;
        }

        info!(agent_id = %agent_id, "Credit line repaid");

        let _ = self.tx.send(DashboardEvent::CreditRepaid { agent_id });

        Ok(())
    }

    /// Get an agent's reputation. Returns a default neutral reputation for unknown agents.
    pub async fn reputation(&self, agent_id: Uuid) -> AgentReputation {
        self.reputations
            .read()
            .await
            .get(&agent_id)
            .cloned()
            .unwrap_or_else(|| {
                AgentReputation { agent_id, ..Default::default() }
            })
    }

    /// Get all registered agents.
    pub async fn get_agents(&self) -> Vec<Agent> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Get all active credit lines.
    pub async fn get_active_lines(&self) -> Vec<CreditLine> {
        self.credit_lines
            .read()
            .await
            .values()
            .filter(|l| l.status == CreditStatus::Active)
            .cloned()
            .collect()
    }

    /// Get all reputations.
    pub async fn get_reputations(&self) -> Vec<AgentReputation> {
        self.reputations.read().await.values().cloned().collect()
    }

    /// Shared read-only handle to credit lines (for guardian).
    pub fn credit_lines_read(&self) -> Arc<RwLock<HashMap<Uuid, CreditLine>>> {
        Arc::clone(&self.credit_lines)
    }

    // -----------------------------------------------------------------------
    // Private scoring helpers
    // -----------------------------------------------------------------------

    /// Compute the deterministic score for a proposal.
    fn compute_score(&self, proposal: &CreditProposal, reputation: &AgentReputation) -> f64 {
        let strategy_clarity = self.score_strategy_clarity(proposal);
        let risk_return = self.score_risk_return(proposal);
        let rep_score = reputation.score;
        let collateral = self.score_collateral(proposal);

        strategy_clarity * 0.30 + risk_return * 0.25 + rep_score * 0.30 + collateral * 0.15
    }

    /// Score strategy clarity (0–10). Longer, more specific strategies score higher.
    fn score_strategy_clarity(&self, proposal: &CreditProposal) -> f64 {
        let len = proposal.strategy.len();
        let has_pairs = !proposal.allowed_pairs.is_empty();
        let has_limits = proposal.max_single_trade_usd > 0.0 && proposal.max_loss_usd > 0.0;

        let mut score: f64 = 0.0;

        // Strategy length: short = low clarity
        if len > 200 {
            score += 4.0;
        } else if len > 100 {
            score += 3.0;
        } else if len > 50 {
            score += 2.0;
        } else {
            score += 1.0;
        }

        if has_pairs {
            score += 3.0;
        }
        if has_limits {
            score += 3.0;
        }

        score.min(10.0)
    }

    /// Score risk/return ratio (0–10). Realistic targets with tight stop-losses score higher.
    fn score_risk_return(&self, proposal: &CreditProposal) -> f64 {
        if proposal.max_loss_usd <= 0.0 || proposal.target_return_pct <= 0.0 {
            return 1.0;
        }

        let max_loss_ratio = proposal.max_loss_usd / proposal.requested_usd;
        let return_ratio = proposal.target_return_pct / 100.0;

        // Reward tight stop-losses (< 20% of requested)
        let loss_score = if max_loss_ratio < 0.10 {
            9.0
        } else if max_loss_ratio < 0.20 {
            7.0
        } else if max_loss_ratio < 0.50 {
            5.0
        } else {
            3.0
        };

        // Penalize unrealistic returns (> 50%)
        let return_score = if return_ratio < 0.10 {
            8.0
        } else if return_ratio < 0.25 {
            7.0
        } else if return_ratio < 0.50 {
            5.0
        } else {
            2.0
        };

        (loss_score + return_score) / 2.0
    }

    /// Score collateral quality (0–10). Having collateral is better than not.
    fn score_collateral(&self, proposal: &CreditProposal) -> f64 {
        match &proposal.collateral {
            Some(c) if c.amount > 0.0 => {
                let coverage = c.amount / proposal.requested_usd;
                if coverage >= 1.0 {
                    10.0
                } else if coverage >= 0.50 {
                    7.0
                } else if coverage >= 0.25 {
                    5.0
                } else {
                    3.0
                }
            }
            _ => 2.0, // No collateral: low but not zero
        }
    }

    /// Generate a human-readable rejection reason.
    fn rejection_reason(
        &self,
        proposal: &CreditProposal,
        score: f64,
        reputation: &AgentReputation,
    ) -> String {
        let mut reasons = Vec::new();

        if self.score_strategy_clarity(proposal) < 5.0 {
            reasons.push("strategy lacks clarity or specificity");
        }
        if self.score_risk_return(proposal) < 5.0 {
            reasons.push("risk/return ratio is unfavorable");
        }
        if reputation.score < 5.0 {
            reasons.push("agent reputation is below threshold");
        }
        if reputation.lines_recalled > 0 {
            reasons.push("agent has prior credit recalls");
        }
        if self.score_collateral(proposal) < 3.0 {
            reasons.push("insufficient or no collateral");
        }

        if reasons.is_empty() {
            format!("Overall score {score:.2} below threshold 6.0")
        } else {
            format!(
                "Score {score:.2}/10.0 — {}",
                reasons.join("; ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Collateral, RepaymentTrigger};
    use chrono::Duration;

    fn make_tx() -> broadcast::Sender<DashboardEvent> {
        let (tx, _) = broadcast::channel(100);
        tx
    }

    fn good_proposal(agent_id: Uuid) -> CreditProposal {
        CreditProposal {
            id: Uuid::new_v4(),
            agent_id,
            submitted_at: Utc::now(),
            requested_usd: 1000.0,
            max_loss_usd: 100.0,
            target_return_pct: 10.0,
            window_start: Utc::now(),
            window_end: Utc::now() + Duration::hours(24),
            strategy: "Buy BTC-USDT on dip using RSI oversold signal below 30, with a tight stop-loss at 10% of capital. Target 10% return within 24h window based on historical mean reversion patterns on 4h timeframe.".to_string(),
            allowed_pairs: vec!["BTC-USDT".to_string()],
            max_single_trade_usd: 500.0,
            repayment_trigger: RepaymentTrigger::ProfitTarget { pct: 10.0 },
            collateral: Some(Collateral {
                asset: "USDT".to_string(),
                amount: 500.0,
                locked_at: Utc::now(),
            }),
        }
    }

    fn bad_proposal(agent_id: Uuid) -> CreditProposal {
        CreditProposal {
            id: Uuid::new_v4(),
            agent_id,
            submitted_at: Utc::now(),
            requested_usd: 100_000.0,
            max_loss_usd: 80_000.0,
            target_return_pct: 500.0,
            window_start: Utc::now(),
            window_end: Utc::now() + Duration::hours(1),
            strategy: "yolo".to_string(),
            allowed_pairs: vec![],
            max_single_trade_usd: 0.0,
            repayment_trigger: RepaymentTrigger::Manual,
            collateral: None,
        }
    }

    #[tokio::test]
    async fn test_register_agent() {
        let banker = Banker::new(make_tx());
        let agent = banker.register_agent("test-agent".to_string()).await;
        assert!(banker.is_registered(agent.id).await);
    }

    #[tokio::test]
    async fn test_good_proposal_approved() {
        let banker = Banker::new(make_tx());
        let agent = banker.register_agent("good-agent".to_string()).await;
        let proposal = good_proposal(agent.id);
        let decision = banker.evaluate(&proposal).await;
        assert!(decision.approved);
        assert!(decision.approved_usd.is_some());
        assert!(decision.score >= 6.0);
    }

    #[tokio::test]
    async fn test_bad_proposal_rejected() {
        let banker = Banker::new(make_tx());
        let agent = banker.register_agent("bad-agent".to_string()).await;
        let proposal = bad_proposal(agent.id);
        let decision = banker.evaluate(&proposal).await;
        assert!(!decision.approved);
        assert!(decision.rejection_reason.is_some());
        assert!(decision.score < 6.0);
    }

    #[tokio::test]
    async fn test_deduct_and_repay() {
        let banker = Banker::new(make_tx());
        let agent = banker.register_agent("deduct-agent".to_string()).await;
        let proposal = good_proposal(agent.id);
        let decision = banker.evaluate(&proposal).await;
        assert!(decision.approved);

        let approved = decision.approved_usd.expect("approved");
        banker.deduct(agent.id, approved * 0.5).await.expect("deduct");

        let line = banker.get_active_line(agent.id).await.expect("line");
        assert!(line.spent_usd > 0.0);

        banker.repay(agent.id).await.expect("repay");
        assert!(banker.get_active_line(agent.id).await.is_none());
    }

    #[tokio::test]
    async fn test_recall() {
        let banker = Banker::new(make_tx());
        let agent = banker.register_agent("recall-agent".to_string()).await;
        let proposal = good_proposal(agent.id);
        banker.evaluate(&proposal).await;

        banker
            .recall(agent.id, "max loss exceeded".to_string())
            .await
            .expect("recall");

        assert!(banker.get_active_line(agent.id).await.is_none());

        let rep = banker.reputation(agent.id).await;
        assert_eq!(rep.lines_recalled, 1);
        assert!(rep.score < 5.0);
    }
}
