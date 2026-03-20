//! In-memory state store for the system.
//!
//! Tracks recent proposals, guardian results, portfolio state, and produces
//! `DashboardSnapshot` for the API. Production: back with Redis.

use crate::types::{
    Agent, AgentReputation, CreditLine, DashboardSnapshot, GuardianResult, TradeProposal,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

const MAX_RECENT_PROPOSALS: usize = 100;
const MAX_RECENT_RESULTS: usize = 100;

/// In-memory state store. Thread-safe via `RwLock`.
pub struct Monitor {
    recent_proposals: Arc<RwLock<Vec<TradeProposal>>>,
    recent_results: Arc<RwLock<Vec<GuardianResult>>>,
    portfolio: Arc<RwLock<HashMap<String, f64>>>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    /// Create a new Monitor.
    pub fn new() -> Self {
        Self {
            recent_proposals: Arc::new(RwLock::new(Vec::new())),
            recent_results: Arc::new(RwLock::new(Vec::new())),
            portfolio: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record a trade proposal.
    pub async fn record_proposal(&self, proposal: TradeProposal) {
        let mut proposals = self.recent_proposals.write().await;
        proposals.push(proposal);
        if proposals.len() > MAX_RECENT_PROPOSALS {
            proposals.remove(0);
        }
    }

    /// Record a guardian result.
    pub async fn record_guardian_result(&self, result: GuardianResult) {
        let mut results = self.recent_results.write().await;
        results.push(result);
        if results.len() > MAX_RECENT_RESULTS {
            results.remove(0);
        }
    }

    /// Update portfolio balances from OKX poller.
    pub async fn update_portfolio(&self, balances: HashMap<String, f64>) {
        let mut portfolio = self.portfolio.write().await;
        *portfolio = balances;
        info!("Portfolio updated");
    }

    /// Get current portfolio state.
    pub async fn get_portfolio(&self) -> HashMap<String, f64> {
        self.portfolio.read().await.clone()
    }

    /// Build a full dashboard snapshot.
    pub async fn snapshot(
        &self,
        agents: Vec<Agent>,
        active_credit_lines: Vec<CreditLine>,
        reputations: Vec<AgentReputation>,
    ) -> DashboardSnapshot {
        DashboardSnapshot {
            agents,
            active_credit_lines,
            recent_proposals: self.recent_proposals.read().await.clone(),
            recent_guardian_results: self.recent_results.read().await.clone(),
            portfolio: self.portfolio.read().await.clone(),
            reputations,
            timestamp: Utc::now(),
        }
    }
}
