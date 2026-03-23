//! 6-check risk verification layer.
//!
//! The Guardian runs **before every trade execution**. No trade reaches OKX without
//! passing all 6 checks. The Guardian has **read-only** access to credit lines.
//!
//! Check order (do NOT reorder):
//! 1. `check_credit_line`     — active line? pair allowed? amount within budget? time in window?
//! 2. `check_policy`          — global pair whitelist? global per-trade limit?
//! 3. `check_confidence`      — agent confidence >= 40%?
//! 4. `check_rate_limit`      — under N trades/hour?
//! 5. `check_contract_safety` — for DeFi: contract whitelisted? method safe?
//! 6. `check_anomaly`         — suspicious proposal rate? escalating risk scores?

use crate::types::{
    AppError, CheckResult, CreditLine, CreditStatus, DashboardEvent, GuardianResult, PolicyConfig,
    TradeProposal,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

/// Per-agent rate tracking data.
#[derive(Debug, Clone)]
struct AgentActivity {
    /// Timestamps of recent proposals (for rate limiting).
    recent_proposals: Vec<DateTime<Utc>>,
    /// Rolling cumulative risk score.
    cumulative_risk_score: f64,
}

/// The Guardian verifies every trade proposal against 6 checks.
pub struct Guardian {
    /// Read-only access to credit lines (owned by Banker).
    credit_lines: Arc<RwLock<HashMap<Uuid, CreditLine>>>,
    /// Global policy configuration.
    policy: PolicyConfig,
    /// Per-agent activity tracking for rate limiting and anomaly detection.
    activity: Arc<RwLock<HashMap<Uuid, AgentActivity>>>,
    /// Dashboard event broadcaster.
    tx: broadcast::Sender<DashboardEvent>,
}

impl Guardian {
    /// Create a new Guardian with read-only access to credit lines.
    pub fn new(
        credit_lines: Arc<RwLock<HashMap<Uuid, CreditLine>>>,
        policy: PolicyConfig,
        tx: broadcast::Sender<DashboardEvent>,
    ) -> Self {
        Self {
            credit_lines,
            policy,
            activity: Arc::new(RwLock::new(HashMap::new())),
            tx,
        }
    }

    /// Run all 6 checks on a trade proposal. Returns a GuardianResult with per-check audit log.
    ///
    /// **All checks run regardless of prior failures** to produce a complete audit log.
    /// The proposal is approved only if every check passes.
    pub async fn verify(&self, proposal: &TradeProposal) -> Result<GuardianResult, AppError> {
        let mut checks = Vec::with_capacity(6);
        let mut composite_risk = 0.0;

        // CHECK 1: Credit line (MUST be first — do not reorder)
        let credit_check = self.check_credit_line(proposal).await;
        if !credit_check.passed {
            composite_risk += 10.0;
        }
        checks.push(credit_check);

        // CHECK 2: Global policy
        let policy_check = self.check_policy(proposal);
        if !policy_check.passed {
            composite_risk += 5.0;
        }
        checks.push(policy_check);

        // CHECK 3: Confidence
        let confidence_check = self.check_confidence(proposal);
        if !confidence_check.passed {
            composite_risk += 3.0;
        }
        checks.push(confidence_check);

        // CHECK 4: Rate limit
        let rate_check = self.check_rate_limit(proposal).await;
        if !rate_check.passed {
            composite_risk += 4.0;
        }
        checks.push(rate_check);

        // CHECK 5: Contract safety (DeFi only)
        let contract_check = self.check_contract_safety(proposal);
        if !contract_check.passed {
            composite_risk += 8.0;
        }
        checks.push(contract_check);

        // CHECK 6: Anomaly detection
        let anomaly_check = self.check_anomaly(proposal).await;
        if !anomaly_check.passed {
            composite_risk += 6.0;
        }
        checks.push(anomaly_check);

        let approved = checks.iter().all(|c| c.passed);

        // Record this proposal in activity tracking
        {
            let mut activity = self.activity.write().await;
            let entry = activity
                .entry(proposal.agent_id)
                .or_insert_with(|| AgentActivity {
                    recent_proposals: Vec::new(),
                    cumulative_risk_score: 0.0,
                });
            // Prune timestamps older than 1 hour to prevent unbounded growth
            let one_hour_ago = Utc::now() - chrono::Duration::hours(1);
            entry.recent_proposals.retain(|t| *t > one_hour_ago);
            entry.recent_proposals.push(Utc::now());
            // Decay cumulative risk score by 10% each proposal, then add new risk
            entry.cumulative_risk_score = entry.cumulative_risk_score * 0.9 + composite_risk;
        }

        let result = GuardianResult {
            proposal_id: proposal.id,
            approved,
            risk_score: composite_risk,
            checks,
        };

        if approved {
            info!(
                proposal_id = %proposal.id,
                agent_id = %proposal.agent_id,
                risk_score = composite_risk,
                "Guardian approved proposal"
            );
        } else {
            warn!(
                proposal_id = %proposal.id,
                agent_id = %proposal.agent_id,
                risk_score = composite_risk,
                "Guardian rejected proposal"
            );
        }

        let _ = self.tx.send(DashboardEvent::GuardianVerdict {
            result: result.clone(),
        });

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // CHECK 1: Credit line — ALWAYS FIRST
    // -----------------------------------------------------------------------

    /// Verify the agent has an active credit line that covers this trade.
    async fn check_credit_line(&self, proposal: &TradeProposal) -> CheckResult {
        let lines = self.credit_lines.read().await;
        let line = match lines.get(&proposal.agent_id) {
            Some(l) => l,
            None => {
                return CheckResult {
                    name: "check_credit_line".to_string(),
                    passed: false,
                    detail: "No credit line found for agent".to_string(),
                };
            }
        };

        // Must be active
        if line.status != CreditStatus::Active {
            return CheckResult {
                name: "check_credit_line".to_string(),
                passed: false,
                detail: format!("Credit line status is {:?}, not Active", line.status),
            };
        }

        // Must not be expired
        if Utc::now() > line.expires_at {
            return CheckResult {
                name: "check_credit_line".to_string(),
                passed: false,
                detail: "Credit line has expired".to_string(),
            };
        }

        // Pair must be allowed
        if !line.conditions.allowed_pairs.is_empty()
            && !line.conditions.allowed_pairs.contains(&proposal.pair)
        {
            return CheckResult {
                name: "check_credit_line".to_string(),
                passed: false,
                detail: format!("Pair {} not in allowed pairs", proposal.pair),
            };
        }

        // Amount must be within single trade limit
        if proposal.amount_usd > line.conditions.max_single_trade_usd {
            return CheckResult {
                name: "check_credit_line".to_string(),
                passed: false,
                detail: format!(
                    "Trade ${:.2} exceeds single trade limit ${:.2}",
                    proposal.amount_usd, line.conditions.max_single_trade_usd
                ),
            };
        }

        // Amount must be within remaining budget (buys only — sells return capital to USDT)
        if proposal.side != crate::types::TradeSide::Sell
            && proposal.amount_usd > line.remaining_usd
        {
            return CheckResult {
                name: "check_credit_line".to_string(),
                passed: false,
                detail: format!(
                    "Trade ${:.2} exceeds remaining budget ${:.2}",
                    proposal.amount_usd, line.remaining_usd
                ),
            };
        }

        CheckResult {
            name: "check_credit_line".to_string(),
            passed: true,
            detail: format!(
                "OK — ${:.2} remaining of ${:.2} approved",
                line.remaining_usd, line.approved_usd
            ),
        }
    }

    // -----------------------------------------------------------------------
    // CHECK 2: Global policy
    // -----------------------------------------------------------------------

    /// Verify the trade meets global policy constraints.
    fn check_policy(&self, proposal: &TradeProposal) -> CheckResult {
        // Global pair whitelist
        if !self.policy.allowed_pairs.is_empty()
            && !self.policy.allowed_pairs.contains(&proposal.pair)
        {
            return CheckResult {
                name: "check_policy".to_string(),
                passed: false,
                detail: format!("Pair {} not in global whitelist", proposal.pair),
            };
        }

        // Global per-trade limit
        if proposal.amount_usd > self.policy.max_single_trade_usd {
            return CheckResult {
                name: "check_policy".to_string(),
                passed: false,
                detail: format!(
                    "Trade ${:.2} exceeds global limit ${:.2}",
                    proposal.amount_usd, self.policy.max_single_trade_usd
                ),
            };
        }

        CheckResult {
            name: "check_policy".to_string(),
            passed: true,
            detail: "OK — within global policy".to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // CHECK 3: Confidence
    // -----------------------------------------------------------------------

    /// Verify the agent's confidence meets the minimum threshold.
    fn check_confidence(&self, proposal: &TradeProposal) -> CheckResult {
        if proposal.confidence < self.policy.min_confidence {
            return CheckResult {
                name: "check_confidence".to_string(),
                passed: false,
                detail: format!(
                    "Confidence {:.1}% below minimum {:.1}%",
                    proposal.confidence * 100.0,
                    self.policy.min_confidence * 100.0
                ),
            };
        }

        CheckResult {
            name: "check_confidence".to_string(),
            passed: true,
            detail: format!("OK — confidence {:.1}%", proposal.confidence * 100.0),
        }
    }

    // -----------------------------------------------------------------------
    // CHECK 4: Rate limit
    // -----------------------------------------------------------------------

    /// Verify the agent hasn't exceeded the rate limit.
    async fn check_rate_limit(&self, proposal: &TradeProposal) -> CheckResult {
        let activity = self.activity.read().await;
        if let Some(agent_activity) = activity.get(&proposal.agent_id) {
            let one_hour_ago = Utc::now() - chrono::Duration::hours(1);
            let recent_count = agent_activity
                .recent_proposals
                .iter()
                .filter(|t| **t > one_hour_ago)
                .count();

            if recent_count >= self.policy.max_trades_per_hour as usize {
                return CheckResult {
                    name: "check_rate_limit".to_string(),
                    passed: false,
                    detail: format!(
                        "{} trades in last hour, limit is {}",
                        recent_count, self.policy.max_trades_per_hour
                    ),
                };
            }

            return CheckResult {
                name: "check_rate_limit".to_string(),
                passed: true,
                detail: format!(
                    "OK — {}/{} trades in last hour",
                    recent_count, self.policy.max_trades_per_hour
                ),
            };
        }

        CheckResult {
            name: "check_rate_limit".to_string(),
            passed: true,
            detail: "OK — first trade".to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // CHECK 5: Contract safety (DeFi only)
    // -----------------------------------------------------------------------

    /// For DeFi trades, verify the contract and method are whitelisted.
    fn check_contract_safety(&self, proposal: &TradeProposal) -> CheckResult {
        // If no contract address, this is a CEX trade — pass automatically
        let contract = match &proposal.contract_address {
            Some(addr) => addr,
            None => {
                return CheckResult {
                    name: "check_contract_safety".to_string(),
                    passed: true,
                    detail: "OK — CEX trade, no contract check needed".to_string(),
                };
            }
        };

        // Contract must be whitelisted
        if !self.policy.whitelisted_contracts.is_empty()
            && !self.policy.whitelisted_contracts.contains(contract)
        {
            return CheckResult {
                name: "check_contract_safety".to_string(),
                passed: false,
                detail: format!("Contract {} not in whitelist", contract),
            };
        }

        // Method must be safe
        if let Some(method) = &proposal.contract_method {
            if !self.policy.safe_methods.is_empty() && !self.policy.safe_methods.contains(method) {
                return CheckResult {
                    name: "check_contract_safety".to_string(),
                    passed: false,
                    detail: format!("Method {} not in safe methods list", method),
                };
            }
        }

        CheckResult {
            name: "check_contract_safety".to_string(),
            passed: true,
            detail: "OK — contract and method whitelisted".to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // CHECK 6: Anomaly detection
    // -----------------------------------------------------------------------

    /// Detect suspicious behaviour: rapid proposal bursts or escalating risk.
    async fn check_anomaly(&self, proposal: &TradeProposal) -> CheckResult {
        let activity = self.activity.read().await;
        if let Some(agent_activity) = activity.get(&proposal.agent_id) {
            // Check proposal rate (per minute)
            let one_min_ago = Utc::now() - chrono::Duration::minutes(1);
            let burst_count = agent_activity
                .recent_proposals
                .iter()
                .filter(|t| **t > one_min_ago)
                .count();

            if burst_count >= self.policy.max_proposals_per_minute as usize {
                return CheckResult {
                    name: "check_anomaly".to_string(),
                    passed: false,
                    detail: format!(
                        "Proposal burst: {} in last minute (limit {})",
                        burst_count, self.policy.max_proposals_per_minute
                    ),
                };
            }

            // Check cumulative risk score
            if agent_activity.cumulative_risk_score > self.policy.max_cumulative_risk_score {
                return CheckResult {
                    name: "check_anomaly".to_string(),
                    passed: false,
                    detail: format!(
                        "Cumulative risk score {:.1} exceeds threshold {:.1}",
                        agent_activity.cumulative_risk_score, self.policy.max_cumulative_risk_score
                    ),
                };
            }
        }

        CheckResult {
            name: "check_anomaly".to_string(),
            passed: true,
            detail: "OK — no anomalies detected".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApprovedConditions, CreditLine, CreditStatus, TradeSide};
    use chrono::Duration;

    fn make_tx() -> broadcast::Sender<DashboardEvent> {
        let (tx, _) = broadcast::channel(100);
        tx
    }

    fn make_credit_line(agent_id: Uuid) -> CreditLine {
        CreditLine {
            id: Uuid::new_v4(),
            proposal_id: Uuid::new_v4(),
            agent_id,
            approved_usd: 5.0,
            spent_usd: 0.0,
            remaining_usd: 5.0,
            status: CreditStatus::Active,
            approved_at: Utc::now(),
            expires_at: Utc::now() + Duration::hours(24),
            conditions: ApprovedConditions {
                allowed_pairs: vec!["BTC-USDT".to_string(), "ETH-USDT".to_string()],
                max_single_trade_usd: 1.0,
                max_loss_usd: 3.0,
                window_end: Utc::now() + Duration::hours(24),
            },
            reputation_at_approval: 5.0,
        }
    }

    fn make_good_proposal(agent_id: Uuid) -> TradeProposal {
        TradeProposal {
            id: Uuid::new_v4(),
            agent_id,
            submitted_at: Utc::now(),
            pair: "BTC-USDT".to_string(),
            side: TradeSide::Buy,
            amount_usd: 0.50,
            confidence: 0.75,
            reasoning: "RSI oversold on 4h timeframe".to_string(),
            contract_address: None,
            contract_method: None,
        }
    }

    #[tokio::test]
    async fn test_good_proposal_passes_all_checks() {
        let agent_id = Uuid::new_v4();
        let credit_lines = Arc::new(RwLock::new(HashMap::new()));
        credit_lines
            .write()
            .await
            .insert(agent_id, make_credit_line(agent_id));

        let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
        let proposal = make_good_proposal(agent_id);
        let result = guardian.verify(&proposal).await.expect("verify");

        assert!(result.approved);
        assert_eq!(result.checks.len(), 6);
        assert!(result.checks.iter().all(|c| c.passed));
    }

    #[tokio::test]
    async fn test_no_credit_line_rejected() {
        let agent_id = Uuid::new_v4();
        let credit_lines = Arc::new(RwLock::new(HashMap::new()));
        // No credit line for this agent

        let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
        let proposal = make_good_proposal(agent_id);
        let result = guardian.verify(&proposal).await.expect("verify");

        assert!(!result.approved);
        assert!(!result.checks[0].passed); // check_credit_line is first
        assert_eq!(result.checks[0].name, "check_credit_line");
    }

    #[tokio::test]
    async fn test_low_confidence_rejected() {
        let agent_id = Uuid::new_v4();
        let credit_lines = Arc::new(RwLock::new(HashMap::new()));
        credit_lines
            .write()
            .await
            .insert(agent_id, make_credit_line(agent_id));

        let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
        let mut proposal = make_good_proposal(agent_id);
        proposal.confidence = 0.20; // below 40% threshold

        let result = guardian.verify(&proposal).await.expect("verify");
        assert!(!result.approved);
        assert!(!result.checks[2].passed); // check_confidence is third
    }

    #[tokio::test]
    async fn test_disallowed_pair_rejected() {
        let agent_id = Uuid::new_v4();
        let credit_lines = Arc::new(RwLock::new(HashMap::new()));
        credit_lines
            .write()
            .await
            .insert(agent_id, make_credit_line(agent_id));

        let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
        let mut proposal = make_good_proposal(agent_id);
        proposal.pair = "DOGE-USDT".to_string(); // not in allowed pairs

        let result = guardian.verify(&proposal).await.expect("verify");
        assert!(!result.approved);
    }

    #[tokio::test]
    async fn test_exceeds_budget_rejected() {
        let agent_id = Uuid::new_v4();
        let credit_lines = Arc::new(RwLock::new(HashMap::new()));
        let mut line = make_credit_line(agent_id);
        line.remaining_usd = 0.30;
        credit_lines.write().await.insert(agent_id, line);

        let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
        let mut proposal = make_good_proposal(agent_id);
        proposal.amount_usd = 0.50; // exceeds remaining $0.30

        let result = guardian.verify(&proposal).await.expect("verify");
        assert!(!result.approved);
        assert!(!result.checks[0].passed);
    }
}
