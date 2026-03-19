//! Proxy to OKX OnchainOS Skills MCP subprocess.
//!
//! Handles DeFi operations: DEX swap across 500+ DEXs, cross-chain bridge,
//! contract calls, broadcasting. Flow: get quote -> simulate -> co-sign -> broadcast -> track.
//! Keys never touch our code — OKX handles signing.

use crate::types::{AppError, TradeProposal};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// OKX OnchainOS DeFi execution proxy.
pub struct OkxOnchainExecutor {
    /// The MCP subprocess handle, if spawned.
    process: Mutex<Option<Child>>,
}

impl OkxOnchainExecutor {
    /// Create a new OKX Onchain executor. Does not start the subprocess yet.
    pub fn new() -> Self {
        Self {
            process: Mutex::new(None),
        }
    }

    /// Attempt to start the `onchainos-skills` subprocess.
    pub async fn start(&self) -> Result<(), AppError> {
        let mut proc = self.process.lock().await;
        if proc.is_some() {
            return Ok(());
        }

        info!("Attempting to start onchainos-skills subprocess");

        match Command::new("onchainos-skills")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                info!(
                    "onchainos-skills subprocess started (pid: {:?})",
                    child.id()
                );
                *proc = Some(child);
                Ok(())
            }
            Err(e) => {
                warn!("Could not start onchainos-skills: {e} — DeFi trades will be simulated");
                Err(AppError::ExecutionFailed(format!(
                    "onchainos-skills not available: {e}"
                )))
            }
        }
    }

    /// Execute a DeFi trade via the OKX OnchainOS Skills subprocess.
    /// If the subprocess is not running, simulates the trade.
    pub async fn execute(&self, proposal: &TradeProposal) -> Result<serde_json::Value, AppError> {
        let proc = self.process.lock().await;
        if proc.is_none() {
            return self.simulate(proposal).await;
        }

        // In a full implementation, the flow is:
        // 1. get_quote -> returns price, route, estimated gas
        // 2. simulate -> dry-run the transaction
        // 3. co-sign -> banker co-signs the UserOp
        // 4. broadcast -> send to chain
        // 5. track -> poll for confirmation

        info!(
            proposal_id = %proposal.id,
            contract = proposal.contract_address.as_deref().unwrap_or("none"),
            "DeFi trade execution initiated"
        );

        // Stub: return simulated response
        self.simulate(proposal).await
    }

    /// Simulate a DeFi trade when OnchainOS is not available.
    async fn simulate(&self, proposal: &TradeProposal) -> Result<serde_json::Value, AppError> {
        warn!(
            proposal_id = %proposal.id,
            "Simulating DeFi trade (onchainos-skills not running)"
        );

        Ok(serde_json::json!({
            "simulated": true,
            "proposal_id": proposal.id,
            "pair": proposal.pair,
            "amount_usd": proposal.amount_usd,
            "contract": proposal.contract_address,
            "method": proposal.contract_method,
            "status": "simulated",
            "message": "DeFi trade simulated — onchainos-skills not available"
        }))
    }
}
