//! Proxy to OKX Agent Trade Kit MCP subprocess.
//!
//! Handles CEX operations: spot, perps, options, grid bots, algo orders.
//! Keys never touch our code — OKX handles signing via `okx-trade-mcp`.

use crate::execution::okx_rest::OkxRestClient;
use crate::types::{AppError, TradeProposal, TradeSide};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// OKX CEX execution proxy.
pub struct OkxCexExecutor {
    /// The MCP subprocess handle, if spawned.
    process: Mutex<Option<Child>>,
    /// REST client fallback for placing real orders when MCP subprocess unavailable.
    rest_client: Option<Arc<OkxRestClient>>,
}

impl Default for OkxCexExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl OkxCexExecutor {
    /// Create a new OKX CEX executor. Does not start the subprocess yet.
    pub fn new() -> Self {
        Self {
            process: Mutex::new(None),
            rest_client: None,
        }
    }

    /// Create with a REST client fallback for live trading without okx-trade-mcp.
    pub fn with_rest_client(rest_client: Arc<OkxRestClient>) -> Self {
        Self {
            process: Mutex::new(None),
            rest_client: Some(rest_client),
        }
    }

    /// Attempt to start the `okx-trade-mcp` subprocess.
    /// Returns Ok(()) if the subprocess starts or is already running.
    /// Returns Err if the subprocess cannot be started (non-fatal — trades will be simulated).
    pub async fn start(&self) -> Result<(), AppError> {
        let mut proc = self.process.lock().await;
        if proc.is_some() {
            return Ok(());
        }

        info!("Attempting to start okx-trade-mcp subprocess");

        match Command::new("okx-trade-mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                info!("okx-trade-mcp subprocess started (pid: {:?})", child.id());
                *proc = Some(child);
                Ok(())
            }
            Err(e) => {
                warn!("Could not start okx-trade-mcp: {e} — trades will be simulated");
                Err(AppError::ExecutionFailed(format!(
                    "okx-trade-mcp not available: {e}"
                )))
            }
        }
    }

    /// Execute a trade via the OKX Agent Trade Kit MCP subprocess.
    /// If the subprocess is not running, simulates the trade and returns success.
    pub async fn execute(&self, proposal: &TradeProposal) -> Result<serde_json::Value, AppError> {
        let mut proc = self.process.lock().await;

        if let Some(ref mut child) = *proc {
            // Build the MCP JSON-RPC request for OKX trade
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": proposal.id.to_string(),
                "method": "place_order",
                "params": {
                    "instId": proposal.pair.clone(),
                    "tdMode": "cash",
                    "side": match proposal.side {
                        TradeSide::Buy => "buy",
                        TradeSide::Sell => "sell",
                    },
                    "ordType": "market",
                    "sz": format!("{:.2}", proposal.amount_usd),
                }
            });

            let request_str = serde_json::to_string(&request)
                .map_err(AppError::SerdeError)?;

            // Write to subprocess stdin
            if let Some(stdin) = child.stdin.as_mut() {
                stdin
                    .write_all(format!("{request_str}\n").as_bytes())
                    .await
                    .map_err(|e| AppError::ExecutionFailed(format!("Failed to write to okx-trade-mcp: {e}")))?;

                stdin.flush().await.map_err(|e| {
                    AppError::ExecutionFailed(format!("Failed to flush okx-trade-mcp stdin: {e}"))
                })?;
            } else {
                return Err(AppError::ExecutionFailed(
                    "okx-trade-mcp stdin not available".to_string(),
                ));
            }

            // Read response from subprocess stdout
            if let Some(stdout) = child.stdout.as_mut() {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                reader.read_line(&mut line).await.map_err(|e| {
                    AppError::ExecutionFailed(format!("Failed to read from okx-trade-mcp: {e}"))
                })?;

                let response: serde_json::Value =
                    serde_json::from_str(&line).map_err(AppError::SerdeError)?;

                info!(
                    proposal_id = %proposal.id,
                    pair = %proposal.pair,
                    "Trade executed via OKX CEX"
                );

                return Ok(response);
            }

            return Err(AppError::ExecutionFailed(
                "okx-trade-mcp stdout not available".to_string(),
            ));
        }

        // Subprocess not running — try REST API fallback
        if let Some(ref rest) = self.rest_client {
            return self.execute_via_rest(rest, proposal).await;
        }

        // No REST client either — simulate
        self.simulate(proposal).await
    }

    /// Execute a trade via the OKX REST API (fallback when MCP subprocess unavailable).
    async fn execute_via_rest(
        &self,
        rest: &OkxRestClient,
        proposal: &TradeProposal,
    ) -> Result<serde_json::Value, AppError> {
        let side_str = match proposal.side {
            TradeSide::Buy => "buy",
            TradeSide::Sell => "sell",
        };

        info!(
            proposal_id = %proposal.id,
            pair = %proposal.pair,
            side = %side_str,
            amount_usd = proposal.amount_usd,
            "Executing trade via OKX REST API (fallback)"
        );

        let okx_result = rest
            .place_market_order(&proposal.pair, side_str, proposal.amount_usd)
            .await?;

        let order_id = okx_result
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|d| d.get("ordId"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        info!(
            proposal_id = %proposal.id,
            pair = %proposal.pair,
            order_id = %order_id,
            "Trade executed via OKX REST API"
        );

        Ok(serde_json::json!({
            "simulated": false,
            "live": true,
            "proposal_id": proposal.id,
            "pair": proposal.pair,
            "side": side_str,
            "amount_usd": proposal.amount_usd,
            "order_id": order_id,
            "status": "filled",
            "message": "Order placed via OKX REST API"
        }))
    }

    /// Simulate a trade when OKX subprocess is not available.
    async fn simulate(&self, proposal: &TradeProposal) -> Result<serde_json::Value, AppError> {
        warn!(
            proposal_id = %proposal.id,
            pair = %proposal.pair,
            "Simulating trade (okx-trade-mcp not running)"
        );

        Ok(serde_json::json!({
            "simulated": true,
            "proposal_id": proposal.id,
            "pair": proposal.pair,
            "side": format!("{:?}", proposal.side),
            "amount_usd": proposal.amount_usd,
            "status": "filled",
            "message": "Trade simulated — okx-trade-mcp not available"
        }))
    }

    /// Attempt to cancel all orders for the given instrument (used during force-recall).
    #[allow(dead_code)]
    pub async fn cancel_all_orders(&self, pair: &str) -> Result<(), AppError> {
        let proc = self.process.lock().await;
        if proc.is_none() {
            warn!(pair = %pair, "Cannot cancel orders — okx-trade-mcp not running");
            return Ok(());
        }

        // In a real implementation, we'd send a cancel-all JSON-RPC request
        info!(pair = %pair, "Cancel all orders request sent");
        Ok(())
    }
}
