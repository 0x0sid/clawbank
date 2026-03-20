//! Entrypoint for openclaw-aibank.
//!
//! Spawns three concurrent tokio tasks:
//! 1. MCP stdio loop (stdin/stdout — never pollute stdout with logs)
//! 2. Axum dashboard server (:3030)
//! 3. OKX portfolio poller (every 30s)
//!
//! All `tracing` output goes to stderr. stdout is reserved for MCP JSON-RPC only.

mod banker;
mod dashboard;
mod execution;
mod guardian;
mod mcp;
mod monitor;
mod types;

use banker::Banker;
use dashboard::{build_router, DashboardState};
use execution::okx_cex::OkxCexExecutor;
use execution::okx_onchain::OkxOnchainExecutor;
use execution::okx_rest::OkxRestClient;
use guardian::Guardian;
use monitor::Monitor;
use types::{DashboardEvent, PolicyConfig};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() {
    // Load .env file if present (non-fatal if missing)
    if let Err(e) = dotenvy::dotenv() {
        eprintln!("Note: .env not loaded ({e}) — using system environment");
    }

    // All logging goes to stderr — stdout is MCP protocol only
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    info!("openclaw-aibank starting");

    // Dashboard event broadcast channel
    let (tx, _) = broadcast::channel::<DashboardEvent>(256);

    // Core components
    let banker = Arc::new(Banker::new(tx.clone()));
    let monitor = Arc::new(Monitor::new());
    let guardian = Arc::new(Guardian::new(
        banker.credit_lines_read(),
        PolicyConfig::default(),
        tx.clone(),
    ));
    let cex_executor = Arc::new(OkxCexExecutor::new());
    let onchain_executor = Arc::new(OkxOnchainExecutor::new());

    // Try to start OKX executors (non-fatal if they fail)
    if let Err(e) = cex_executor.start().await {
        warn!("OKX CEX executor not available: {e}");
    }
    if let Err(e) = onchain_executor.start().await {
        warn!("OKX Onchain executor not available: {e}");
    }

    // Dashboard port from env or default 3030
    let port: u16 = std::env::var("DASHBOARD_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3030);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    // Task 1: MCP stdio loop
    let mcp_banker = Arc::clone(&banker);
    let mcp_guardian = Arc::clone(&guardian);
    let mcp_monitor = Arc::clone(&monitor);
    let mcp_cex = Arc::clone(&cex_executor);
    let mcp_onchain = Arc::clone(&onchain_executor);
    let mcp_tx = tx.clone();

    let mcp_task = tokio::spawn(async move {
        mcp::skill::run_stdio_loop(
            mcp_banker,
            mcp_guardian,
            mcp_monitor,
            mcp_cex,
            mcp_onchain,
            mcp_tx,
        )
        .await;
    });

    // Task 2: Axum dashboard server
    let dashboard_state = DashboardState {
        banker: Arc::clone(&banker),
        monitor: Arc::clone(&monitor),
        tx: tx.clone(),
    };

    let dashboard_task = tokio::spawn(async move {
        let app = build_router(dashboard_state);
        info!("Dashboard listening on http://{addr}");

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to bind dashboard to {addr}: {e}");
                return;
            }
        };

        if let Err(e) = axum::serve(listener, app).await {
            error!("Dashboard server error: {e}");
        }
    });

    // Task 3: OKX portfolio poller (every 30s)
    let poller_monitor = Arc::clone(&monitor);
    let poller_banker = Arc::clone(&banker);
    let poller_tx = tx.clone();
    let okx_rest = Arc::new(OkxRestClient::new());

    let poller_task = tokio::spawn(async move {
        let interval_secs: u64 = std::env::var("RECALL_CHECK_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        info!("Portfolio poller started (interval: {interval_secs}s)");

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;

            // Fetch real balances from OKX (falls back to simulated if no creds)
            let balances = match okx_rest.get_balances().await {
                Ok(b) => b,
                Err(e) => {
                    error!("Portfolio fetch failed: {e}");
                    HashMap::new()
                }
            };

            poller_monitor.update_portfolio(balances.clone()).await;

            let _ = poller_tx.send(DashboardEvent::PortfolioUpdate {
                balances,
                timestamp: chrono::Utc::now(),
            });

            // Fetch open positions for P&L tracking
            let positions = match okx_rest.get_positions().await {
                Ok(p) => p,
                Err(e) => {
                    error!("Position fetch failed: {e}");
                    Vec::new()
                }
            };

            // Check active credit lines for P&L violations
            let active_lines = poller_banker.get_active_lines().await;
            for line in &active_lines {
                // Sum unrealized P&L across all positions for this line's pairs
                let total_loss: f64 = positions
                    .iter()
                    .filter(|p| line.conditions.allowed_pairs.contains(&p.inst_id))
                    .map(|p| p.unrealized_pnl)
                    .filter(|pnl| *pnl < 0.0)
                    .map(|pnl| pnl.abs())
                    .sum();

                let effective_loss = line.spent_usd + total_loss;

                if effective_loss > line.conditions.max_loss_usd {
                    warn!(
                        agent_id = %line.agent_id,
                        effective_loss = effective_loss,
                        max_loss = line.conditions.max_loss_usd,
                        "Max loss exceeded — triggering force recall"
                    );

                    // FORCE RECALL:
                    // 1. Cancel all orders via OKX REST API
                    for pair in &line.conditions.allowed_pairs {
                        if let Err(e) = okx_rest.cancel_all_orders(pair).await {
                            error!(pair = %pair, error = %e, "Failed to cancel orders");
                        }
                    }

                    // 2. Recall the credit line (blocks future proposals)
                    if let Err(e) = poller_banker
                        .recall(
                            line.agent_id,
                            format!(
                                "Max loss exceeded: ${:.2} > ${:.2} (spent=${:.2} + unrealized_loss=${:.2})",
                                effective_loss, line.conditions.max_loss_usd,
                                line.spent_usd, total_loss,
                            ),
                        )
                        .await
                    {
                        error!(agent_id = %line.agent_id, error = %e, "Failed to recall credit line");
                    }
                }
            }
        }
    });

    // Wait for all tasks (MCP loop exits when stdin closes)
    tokio::select! {
        _ = mcp_task => {
            info!("MCP stdio loop ended");
        }
        _ = dashboard_task => {
            info!("Dashboard server ended");
        }
        _ = poller_task => {
            info!("Portfolio poller ended");
        }
    }

    info!("openclaw-aibank shutting down");
}
