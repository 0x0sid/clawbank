//! Axum HTTP + WebSocket dashboard — no build step, inline HTML.
//!
//! Routes:
//! - `GET /`                     — inline HTML dashboard
//! - `GET /ws`                   — WebSocket live event stream
//! - `GET /api/snapshot`         — full state JSON
//! - `POST /api/agent/register`  — register agent via wallet signature

use crate::banker::Banker;
use crate::execution::okx_rest::{OkxCredentials, OkxRestClient};
use crate::monitor::Monitor;
use crate::types::{CreditProposal, DashboardEvent, RepaymentTrigger};
use alloy::primitives::{Address, Signature as EcdsaSignature};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};

/// Shared state for dashboard handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub banker: Arc<Banker>,
    pub monitor: Arc<Monitor>,
    pub tx: broadcast::Sender<DashboardEvent>,
    pub okx_rest: Arc<OkxRestClient>,
    /// Currently connected wallet address (MetaMask/OKX/etc) for MCP bridge
    pub connected_wallet: Arc<RwLock<Option<String>>>,
}

/// Build the Axum router for the dashboard.
pub fn build_router(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/api/snapshot", get(snapshot_handler))
        .route("/api/credit/:proposal_id/approve", post(approve_handler))
        .route("/api/credit/:proposal_id/reject", post(reject_handler))
        .route("/api/x402/:payment_id/approve", post(x402_approve_handler))
        .route("/api/x402/:payment_id/block", post(x402_block_handler))
        .route("/api/agent/register", post(wallet_register_handler))
        .route("/api/okx/connect", post(okx_connect_handler))
        .route("/api/okx/status", get(okx_status_handler))
        .route("/api/okx/trades", get(okx_trades_handler))
        .route("/api/bot/register", post(bot_register_handler))
        .route("/api/bot/request-credit", post(bot_request_credit_handler))
        .route("/api/bot/report", post(bot_report_handler))
        .route("/api/wallet/status", get(wallet_status_handler))
        .route("/api/wallet/connect", post(wallet_connect_handler))
        .with_state(state)
}

/// Serve the inline HTML dashboard.
async fn index_handler() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

/// WebSocket upgrade handler — streams DashboardEvents to connected clients.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// Handle a single WebSocket connection.
async fn handle_ws(mut socket: WebSocket, state: DashboardState) {
    let mut rx = state.tx.subscribe();
    info!("WebSocket client connected");

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(e) => {
                        let json = match serde_json::to_string(&e) {
                            Ok(j) => j,
                            Err(err) => {
                                error!("Failed to serialize dashboard event: {err}");
                                continue;
                            }
                        };
                        if socket.send(Message::Text(json)).await.is_err() {
                            break; // client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        info!("WebSocket client lagged by {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // ignore other messages
                }
            }
        }
    }

    info!("WebSocket client disconnected");
}

/// Serve the full state snapshot as JSON.
async fn snapshot_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let agents = state.banker.get_agents().await;
    let pending = state.banker.get_pending_proposals().await;
    let lines = state.banker.get_active_lines().await;
    let reps = state.banker.get_reputations().await;
    let pending_x402 = state.banker.get_pending_x402().await;
    let snapshot = state
        .monitor
        .snapshot(agents, pending, pending_x402, lines, reps)
        .await;
    Json(snapshot)
}

/// Approve a pending credit proposal via dashboard.
async fn approve_handler(
    Path(proposal_id): Path<String>,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    let id = match proposal_id.parse::<uuid::Uuid>() {
        Ok(id) => id,
        Err(_) => {
            return Json(serde_json::json!({"error": "Invalid proposal ID"}));
        }
    };

    match state.banker.approve_proposal(id, None).await {
        Ok(credit_line) => {
            info!(proposal_id = %id, "Credit proposal approved via dashboard");
            Json(serde_json::json!({
                "ok": true,
                "credit_line_id": credit_line.id,
                "approved_usd": credit_line.approved_usd,
            }))
        }
        Err(e) => {
            warn!(proposal_id = %id, error = %e, "Failed to approve proposal");
            Json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Reject a pending credit proposal via dashboard.
async fn reject_handler(
    Path(proposal_id): Path<String>,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    let id = match proposal_id.parse::<uuid::Uuid>() {
        Ok(id) => id,
        Err(_) => {
            return Json(serde_json::json!({"error": "Invalid proposal ID"}));
        }
    };

    match state.banker.reject_proposal(id).await {
        Ok(()) => {
            info!(proposal_id = %id, "Credit proposal rejected via dashboard");
            Json(serde_json::json!({"ok": true}))
        }
        Err(e) => {
            warn!(proposal_id = %id, error = %e, "Failed to reject proposal");
            Json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Approve a pending x402 payment via dashboard.
async fn x402_approve_handler(
    Path(payment_id): Path<String>,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    let id = match payment_id.parse::<uuid::Uuid>() {
        Ok(id) => id,
        Err(_) => {
            return Json(serde_json::json!({"error": "Invalid payment ID"}));
        }
    };

    match state.banker.approve_x402(id).await {
        Ok(()) => {
            info!(payment_id = %id, "x402 payment approved via dashboard");
            Json(serde_json::json!({"ok": true}))
        }
        Err(e) => {
            warn!(payment_id = %id, error = %e, "Failed to approve x402 payment");
            Json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Block a pending x402 payment via dashboard.
async fn x402_block_handler(
    Path(payment_id): Path<String>,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    let id = match payment_id.parse::<uuid::Uuid>() {
        Ok(id) => id,
        Err(_) => {
            return Json(serde_json::json!({"error": "Invalid payment ID"}));
        }
    };

    match state
        .banker
        .block_x402(id, "Blocked by human via dashboard".to_string())
        .await
    {
        Ok(()) => {
            info!(payment_id = %id, "x402 payment blocked via dashboard");
            Json(serde_json::json!({"ok": true}))
        }
        Err(e) => {
            warn!(payment_id = %id, error = %e, "Failed to block x402 payment");
            Json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Register an agent via wallet signature (called from dashboard wallet connect).
///
/// Expects JSON body: `{ "name": "...", "evm_address": "0x...", "signature": "0x..." }`
/// The signature must be a `personal_sign` of the message:
///   `"Register as OpenClaw agent: <name>"`
async fn wallet_register_handler(
    State(state): State<DashboardState>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => return Json(serde_json::json!({"error": "Missing or empty 'name'"})),
    };
    let evm_address = match body.get("evm_address").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return Json(serde_json::json!({"error": "Missing 'evm_address'"})),
    };
    let sig_hex = match body.get("signature").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return Json(serde_json::json!({"error": "Missing 'signature'"})),
    };

    // Parse the claimed address
    let claimed: Address = match evm_address.parse() {
        Ok(a) => a,
        Err(_) => return Json(serde_json::json!({"error": "Invalid EVM address format"})),
    };

    // Parse the signature (65 bytes: r[32] + s[32] + v[1])
    let sig_bytes = match hex::decode(sig_hex.strip_prefix("0x").unwrap_or(&sig_hex)) {
        Ok(b) if b.len() == 65 => b,
        _ => return Json(serde_json::json!({"error": "Invalid signature (expected 65-byte hex)"})),
    };

    let signature = match EcdsaSignature::from_raw(&sig_bytes) {
        Ok(s) => s,
        Err(_) => return Json(serde_json::json!({"error": "Malformed ECDSA signature"})),
    };

    // Verify: recover address from personal_sign message
    let message = format!("Register as OpenClaw agent: {name}");
    let recovered = match signature.recover_address_from_msg(message.as_bytes()) {
        Ok(addr) => addr,
        Err(e) => {
            warn!(error = %e, "Wallet registration: signature recovery failed");
            return Json(serde_json::json!({"error": "Signature verification failed"}));
        }
    };

    if recovered != claimed {
        warn!(
            claimed = %claimed,
            recovered = %recovered,
            "Wallet registration: address mismatch"
        );
        return Json(serde_json::json!({"error": "Signature does not match claimed address"}));
    }

    // Signature valid — register the agent with the verified EVM address
    let agent = state
        .banker
        .register_agent(name.clone(), Some(evm_address.clone()))
        .await;

    info!(
        agent_id = %agent.id,
        name = %name,
        evm_address = %evm_address,
        "Agent registered via wallet signature"
    );

    Json(serde_json::json!({
        "ok": true,
        "agent": agent,
    }))
}

/// Connect OKX CEX account via dashboard (API key, secret, passphrase).
///
/// Expects JSON body: `{ "api_key": "...", "secret_key": "...", "passphrase": "..." }`
/// Validates credentials by making a test balance request to OKX.
async fn okx_connect_handler(
    State(state): State<DashboardState>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    let api_key = match body.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return Json(serde_json::json!({"error": "Missing or empty 'api_key'"})),
    };
    let secret_key = match body.get("secret_key").and_then(|v| v.as_str()) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return Json(serde_json::json!({"error": "Missing or empty 'secret_key'"})),
    };
    let passphrase = match body.get("passphrase").and_then(|v| v.as_str()) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return Json(serde_json::json!({"error": "Missing or empty 'passphrase'"})),
    };

    let creds = OkxCredentials {
        api_key: api_key.clone(),
        secret_key,
        passphrase,
    };

    // Save credentials temporarily for the verification test
    let previous = state.okx_rest.take_credentials().await;
    state.okx_rest.set_credentials(creds).await;

    // Verify credentials by attempting a balance fetch
    match state.okx_rest.get_balances().await {
        Ok(balances) => {
            info!(
                assets = balances.len(),
                "OKX CEX account connected via dashboard"
            );
            Json(serde_json::json!({
                "ok": true,
                "message": "OKX account connected",
                "api_key_prefix": format!("{}...", &api_key[..api_key.len().min(6)]),
                "assets": balances.len(),
            }))
        }
        Err(e) => {
            // Revert to previous credentials on failure
            warn!(error = %e, "OKX credential verification failed — reverting");
            state.okx_rest.restore_credentials(previous).await;
            Json(serde_json::json!({
                "error": format!("OKX verification failed: {e}. Credentials not saved."),
            }))
        }
    }
}

/// Check OKX CEX connection status, returns masked key prefix.
async fn okx_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let key_preview = state.okx_rest.api_key_preview().await;
    Json(serde_json::json!({
        "connected": key_preview.is_some(),
        "api_key_preview": key_preview,
    }))
}

/// Fetch recent OKX trade history (live or simulated).
async fn okx_trades_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.okx_rest.get_recent_trades().await {
        Ok(trades) => Json(serde_json::json!({
            "ok": true,
            "simulated": state.okx_rest.api_key_preview().await.is_none(),
            "trades": trades,
        })),
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "error": e.to_string(),
            "trades": [],
        })),
    }
}

/// Register an agent via simple HTTP (no wallet signature needed — for bots).
async fn bot_register_handler(
    State(state): State<DashboardState>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => return Json(serde_json::json!({"error": "Missing 'name'"})),
    };

    let agent = state.banker.register_agent(name, None).await;
    info!(agent_id = %agent.id, name = %agent.name, "Bot agent registered via HTTP");

    Json(serde_json::json!({
        "ok": true,
        "agent_id": agent.id,
        "name": agent.name,
    }))
}

/// Request a credit line for a bot agent (queued for human approval on dashboard).
async fn bot_request_credit_handler(
    State(state): State<DashboardState>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    let agent_id = match body
        .get("agent_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return Json(serde_json::json!({"error": "Missing or invalid 'agent_id'"})),
    };

    if !state.banker.is_registered(agent_id).await {
        return Json(serde_json::json!({"error": format!("Agent {agent_id} not registered")}));
    }

    let requested_usd = body
        .get("amount_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(10.0);
    let strategy = body
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("MACD")
        .to_string();
    let duration_hours = body
        .get("duration_hours")
        .and_then(|v| v.as_f64())
        .unwrap_or(24.0);

    let now = chrono::Utc::now();
    let window_end = now + chrono::Duration::hours(duration_hours as i64);

    let proposal = CreditProposal {
        id: uuid::Uuid::new_v4(),
        agent_id,
        submitted_at: now,
        requested_usd,
        max_loss_usd: requested_usd * 0.1,
        target_return_pct: 5.0,
        window_start: now,
        window_end,
        strategy,
        allowed_pairs: vec!["BTC-USDT".to_string()],
        max_single_trade_usd: 1.0,
        repayment_trigger: RepaymentTrigger::TimeExpiry,
        collateral: None,
    };

    let decision = state.banker.evaluate(&proposal).await;

    info!(
        agent_id = %agent_id,
        proposal_id = %proposal.id,
        "Bot credit request submitted — pending human approval"
    );

    Json(serde_json::json!({
        "ok": true,
        "proposal_id": proposal.id,
        "status": "pending_approval",
        "decision": decision,
        "message": "Credit proposal submitted — approve it on the dashboard",
    }))
}

/// Bot reports simulation progress (broadcast to dashboard as live event).
async fn bot_report_handler(
    State(state): State<DashboardState>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    let agent_id = body
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Broadcast as a generic dashboard event so it shows in Live Events
    let _ = state.tx.send(DashboardEvent::GenericLog {
        source: format!("bot:{}", &agent_id[..agent_id.len().min(8)]),
        message: message.clone(),
    });

    Json(serde_json::json!({"ok": true}))
}

/// Get the currently connected wallet address (for MCP bridge).
async fn wallet_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let wallet = state.connected_wallet.read().await.clone();
    Json(serde_json::json!({
        "connected": wallet.is_some(),
        "address": wallet,
    }))
}

/// Report a connected wallet from the dashboard UI to the backend.
async fn wallet_connect_handler(
    State(state): State<DashboardState>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> impl IntoResponse {
    let address = body
        .get("address")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut wallet = state.connected_wallet.write().await;
    *wallet = address.clone();

    if let Some(ref addr) = address {
        info!(address = %addr, "Wallet connected via dashboard");
    }

    Json(serde_json::json!({
        "ok": true,
        "address": address,
    }))
}

/// Inline HTML for the dashboard — no build step required.
const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>OpenClaw AI Bank</title>
<style>
  :root {
    --bg: #0a0e17; --surface: #111827; --border: #1f2937;
    --text: #e5e7eb; --muted: #9ca3af; --accent: #3b82f6;
    --green: #10b981; --red: #ef4444; --yellow: #f59e0b;
    --font: 'Segoe UI', system-ui, -apple-system, sans-serif;
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { background: var(--bg); color: var(--text); font-family: var(--font); font-size: 14px; line-height: 1.5; }
  header { background: var(--surface); border-bottom: 1px solid var(--border); padding: 16px 24px; display: flex; align-items: center; gap: 16px; }
  header h1 { font-size: 18px; font-weight: 600; }
  .status-dot { width: 10px; height: 10px; border-radius: 50%; background: var(--red); display: inline-block; }
  .status-dot.connected { background: var(--green); }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; padding: 24px; max-width: 1400px; margin: 0 auto; }
  .panel { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px; min-height: 120px; }
  .panel h2 { font-size: 13px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); margin-bottom: 12px; font-weight: 600; }
  .full-width { grid-column: 1 / -1; }
  .event-list { max-height: 350px; overflow-y: auto; }
  .event-item { padding: 8px 12px; border-bottom: 1px solid var(--border); font-size: 13px; font-family: 'Consolas', monospace; }
  .event-item:last-child { border-bottom: none; }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; text-transform: uppercase; }
  .badge.approved { background: rgba(16,185,129,0.2); color: var(--green); }
  .badge.rejected { background: rgba(239,68,68,0.2); color: var(--red); }
  .badge.active { background: rgba(59,130,246,0.2); color: var(--accent); }
  .badge.pending { background: rgba(245,158,11,0.2); color: var(--yellow); }
  .badge.recalled { background: rgba(245,158,11,0.2); color: var(--yellow); }
  .credit-bar { height: 8px; border-radius: 4px; background: var(--border); margin-top: 8px; overflow: hidden; }
  .credit-bar-fill { height: 100%; border-radius: 4px; background: var(--accent); transition: width 0.3s ease; }
  .stat { text-align: center; padding: 12px; }
  .stat-value { font-size: 24px; font-weight: 700; color: var(--accent); }
  .stat-label { font-size: 11px; color: var(--muted); text-transform: uppercase; }
  .stats-row { display: flex; gap: 24px; flex-wrap: wrap; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th { text-align: left; color: var(--muted); font-weight: 600; padding: 8px; border-bottom: 1px solid var(--border); }
  td { padding: 8px; border-bottom: 1px solid var(--border); }
  .empty { color: var(--muted); font-style: italic; padding: 24px; text-align: center; }
  .btn { padding: 6px 16px; border: none; border-radius: 6px; font-size: 13px; font-weight: 600; cursor: pointer; transition: all 0.15s; }
  .btn:hover { filter: brightness(1.15); }
  .btn-approve { background: var(--green); color: #fff; }
  .btn-reject { background: var(--red); color: #fff; margin-left: 8px; }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .proposal-card { background: var(--bg); border: 1px solid var(--border); border-radius: 8px; padding: 14px; margin-bottom: 12px; }
  .proposal-card .score { font-size: 22px; font-weight: 700; float: right; }
  .proposal-card .score.high { color: var(--green); }
  .proposal-card .score.mid { color: var(--yellow); }
  .proposal-card .score.low { color: var(--red); }
  .proposal-card .meta { color: var(--muted); font-size: 12px; margin: 4px 0 10px; }
  .proposal-card .actions { margin-top: 10px; }
  .wallet-bar { margin-left: auto; display: flex; align-items: center; gap: 12px; }
  .wallet-addr { font-family: 'Consolas', monospace; font-size: 12px; color: var(--green); background: rgba(16,185,129,0.1); padding: 4px 10px; border-radius: 6px; }
  .btn-wallet { padding: 8px 18px; border: 1px solid var(--accent); background: transparent; color: var(--accent); border-radius: 6px; font-size: 13px; font-weight: 600; cursor: pointer; transition: all 0.15s; }
  .btn-wallet:hover { background: var(--accent); color: #fff; }
  .btn-wallet.connected { border-color: var(--green); color: var(--green); }
  .btn-wallet.connected:hover { background: var(--green); color: #fff; }
  .register-form { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .register-form input { background: var(--bg); border: 1px solid var(--border); color: var(--text); padding: 8px 12px; border-radius: 6px; font-size: 13px; min-width: 200px; }
  .register-form input:focus { outline: none; border-color: var(--accent); }
  .register-result { margin-top: 10px; padding: 10px 14px; border-radius: 6px; font-size: 13px; display: none; }
  .register-result.success { display: block; background: rgba(16,185,129,0.1); color: var(--green); border: 1px solid rgba(16,185,129,0.3); }
  .register-result.error { display: block; background: rgba(239,68,68,0.1); color: var(--red); border: 1px solid rgba(239,68,68,0.3); }
  .okx-form { display: grid; grid-template-columns: 1fr 1fr 1fr auto; gap: 10px; align-items: center; }
  .okx-form input { background: var(--bg); border: 1px solid var(--border); color: var(--text); padding: 8px 12px; border-radius: 6px; font-size: 13px; }
  .okx-form input:focus { outline: none; border-color: var(--accent); }
  .okx-status { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; padding: 4px 10px; border-radius: 6px; }
  .okx-status.live { color: var(--green); background: rgba(16,185,129,0.1); }
  .okx-status.sim { color: var(--yellow); background: rgba(245,158,11,0.1); }
  .trade-side-buy { color: var(--green); font-weight: 600; }
  .trade-side-sell { color: var(--red); font-weight: 600; }
  .sim-badge { font-size: 10px; color: var(--yellow); background: rgba(245,158,11,0.1); padding: 2px 8px; border-radius: 4px; margin-left: 8px; vertical-align: middle; }
  .btn-wallet.disabled { opacity: 0.4; cursor: not-allowed; border-color: var(--border); color: var(--muted); }
  .btn-wallet.disabled:hover { background: transparent; color: var(--muted); }
</style>
</head>
<body>
<header>
  <h1>OpenClaw AI Bank</h1>
  <span class="status-dot" id="ws-status"></span>
  <span id="ws-label" style="color:var(--muted);font-size:12px;">Disconnected</span>
  <div class="wallet-bar">
    <span id="wallet-addr" class="wallet-addr" style="display:none;"></span>
    <button class="btn-wallet" id="btn-connect" onclick="connectWallet()">Connect Wallet</button>
    <span id="wallet-no-ext" style="display:none;color:var(--muted);font-size:11px;">No wallet extension</span>
  </div>
</header>

<div class="grid">
  <div class="panel full-width">
    <h2>Pending Budget Proposals (requires your approval)</h2>
    <div id="pending-panel"><div class="empty">No pending proposals</div></div>
  </div>

  <div class="panel full-width">
    <h2>Pending x402 Payments (requires your review)</h2>
    <div id="x402-panel"><div class="empty">No pending x402 payments</div></div>
  </div>

  <div class="panel full-width" id="register-panel" style="display:none;">
    <h2>Register as Agent</h2>
    <p style="color:var(--muted);font-size:13px;margin-bottom:12px;">Connected wallet will be your agent address. Sign a message to prove ownership.</p>
    <div class="register-form">
      <input type="text" id="agent-name" placeholder="Agent display name" maxlength="64" />
      <button class="btn btn-approve" onclick="registerAgent()" id="btn-register">Sign &amp; Register</button>
    </div>
    <div class="register-result" id="register-result"></div>
  </div>

  <div class="panel">
    <h2>Agents</h2>
    <div id="agents-panel"><div class="empty">No agents registered</div></div>
  </div>

  <div class="panel">
    <h2>Active Credit Lines</h2>
    <div id="credit-panel"><div class="empty">No active credit lines</div></div>
  </div>

  <div class="panel">
    <h2>Portfolio (OKX)</h2>
    <div id="portfolio-panel"><div class="empty">No portfolio data</div></div>
  </div>

  <div class="panel full-width">
    <h2>OKX Trade History <span class="sim-badge" id="trade-sim-badge" style="display:none;">SIMULATED</span></h2>
    <div id="trades-panel"><div class="empty">Loading trades...</div></div>
  </div>

  <div class="panel">
    <h2>Statistics</h2>
    <div class="stats-row" id="stats-panel">
      <div class="stat"><div class="stat-value" id="stat-trades">0</div><div class="stat-label">Trades</div></div>
      <div class="stat"><div class="stat-value" id="stat-approved">0</div><div class="stat-label">Approved</div></div>
      <div class="stat"><div class="stat-value" id="stat-rejected">0</div><div class="stat-label">Rejected</div></div>
      <div class="stat"><div class="stat-value" id="stat-recalls">0</div><div class="stat-label">Recalls</div></div>
    </div>
  </div>

  <div class="panel full-width">
    <h2>Settings</h2>
    <div style="margin-bottom:14px;">
      <h3 style="font-size:13px;color:var(--muted);text-transform:uppercase;letter-spacing:0.05em;margin-bottom:10px;">OKX CEX API <span class="okx-status sim" id="okx-status">Simulated</span></h3>
      <div class="okx-form">
        <input type="text" id="okx-api-key" placeholder="API Key" autocomplete="off" />
        <input type="password" id="okx-secret-key" placeholder="Secret Key" autocomplete="off" />
        <input type="password" id="okx-passphrase" placeholder="Passphrase" autocomplete="off" />
        <button class="btn btn-approve" onclick="saveOkxKeys()" id="btn-okx-save">Save</button>
      </div>
      <div class="register-result" id="okx-result"></div>
    </div>
  </div>

  <div class="panel full-width">
    <h2>Live Events</h2>
    <div class="event-list" id="event-list"><div class="empty">Waiting for events...</div></div>
  </div>
</div>

<script>
const state = { trades: 0, approved: 0, rejected: 0, recalls: 0 };
let ws;
let walletAddress = null;

// ---------- Wallet Detection ----------
(function detectWallet() {
  const btn = document.getElementById('btn-connect');
  // Check for any wallet: MetaMask (ethereum), OKX Wallet (okexchain), or generic ethereum
  const hasWallet = typeof window.ethereum !== 'undefined' || typeof window.okxwallet !== 'undefined' || typeof window.okexchain !== 'undefined';
  if (!hasWallet) {
    btn.classList.add('disabled');
    btn.textContent = 'No Wallet';
    btn.title = 'Install MetaMask, OKX Wallet, or Rabby browser extension';
    document.getElementById('wallet-no-ext').style.display = 'inline';
  }
})();

// ---------- Wallet Connect ----------

async function connectWallet() {
  // Try OKX Wallet first, then generic ethereum
  const provider = window.okxwallet || window.okexchain || window.ethereum;
  if (!provider) {
    alert('No Web3 wallet detected.\nInstall MetaMask, OKX Wallet, or Rabby browser extension.');
    return;
  }
  try {
    const accounts = await provider.request({ method: 'eth_requestAccounts' });
    if (!accounts || accounts.length === 0) {
      alert('No accounts found in your wallet.\nCreate or import an account first.');
      return;
    }
    walletAddress = accounts[0];
    const short = walletAddress.substring(0, 6) + '...' + walletAddress.substring(38);
    document.getElementById('wallet-addr').textContent = short;
    document.getElementById('wallet-addr').style.display = 'inline-block';
    document.getElementById('btn-connect').textContent = 'Connected';
    document.getElementById('btn-connect').classList.add('connected');
    document.getElementById('register-panel').style.display = 'block';
    
    // Report connected wallet to backend for MCP bridge
    fetch('/api/wallet/connect', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ address: walletAddress })
    }).catch(e => console.error('Failed to report wallet:', e));
  } catch (e) {
    console.error('Wallet connect failed:', e);
    const msg = (e.message || String(e)).toLowerCase();
    if (msg.includes('at least one account')) {
      alert('Your wallet has no accounts yet.\n\nTo fix:\n1. Click the wallet icon in your browser toolbar\n2. Create a new wallet or import one\n3. Then come back here and click Connect Wallet again');
    } else if (e.code === 4001) {
      alert('Connection request was rejected.');
    } else {
      alert('Wallet connect failed: ' + (e.message || e));
    }
  }
}

// Listen for account changes
const walletProvider = window.okxwallet || window.okexchain || window.ethereum;
if (walletProvider) {
  walletProvider.on('accountsChanged', async (accounts) => {
    if (accounts.length === 0) {
      walletAddress = null;
      document.getElementById('wallet-addr').style.display = 'none';
      document.getElementById('btn-connect').textContent = 'Connect Wallet';
      document.getElementById('btn-connect').classList.remove('connected');
      document.getElementById('register-panel').style.display = 'none';
      // Report disconnected wallet to backend
      await fetch('/api/wallet/connect', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ address: null })
      }).catch(e => console.error('Failed to report wallet disconnect:', e));
    } else {
      walletAddress = accounts[0];
      const short = walletAddress.substring(0, 6) + '...' + walletAddress.substring(38);
      document.getElementById('wallet-addr').textContent = short;
    }
  });
}

async function registerAgent() {
  const name = document.getElementById('agent-name').value.trim();
  const result = document.getElementById('register-result');
  result.className = 'register-result';
  result.style.display = 'none';

  if (!walletAddress) { showResult(result, 'error', 'Connect your wallet first.'); return; }
  if (!name) { showResult(result, 'error', 'Enter an agent name.'); return; }

  const btn = document.getElementById('btn-register');
  btn.disabled = true;
  btn.textContent = 'Signing...';

  const provider = window.okxwallet || window.okexchain || window.ethereum;

  try {
    const message = 'Register as OpenClaw agent: ' + name;
    const signature = await provider.request({
      method: 'personal_sign',
      params: [message, walletAddress],
    });

    btn.textContent = 'Registering...';

    const resp = await fetch('/api/agent/register', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, evm_address: walletAddress, signature }),
    });
    const data = await resp.json();

    if (data.ok) {
      showResult(result, 'success',
        'Registered! Agent ID: ' + data.agent.id + ' | Address: ' + walletAddress);
      document.getElementById('agent-name').value = '';
      fetchSnapshot();
    } else {
      showResult(result, 'error', data.error || 'Registration failed');
    }
  } catch (e) {
    if (e.code === 4001) {
      showResult(result, 'error', 'Signature rejected by user.');
    } else {
      showResult(result, 'error', 'Error: ' + (e.message || e));
    }
  }
  btn.disabled = false;
  btn.textContent = 'Sign & Register';
}

function showResult(el, type, msg) {
  el.className = 'register-result ' + type;
  el.textContent = msg;
  el.style.display = 'block';
}

// ---------- OKX Settings ----------

async function loadOkxStatus() {
  try {
    const resp = await fetch('/api/okx/status');
    const data = await resp.json();
    setOkxStatus(data.connected, data.api_key_preview);
    if (data.connected) {
      document.getElementById('okx-api-key').placeholder = data.api_key_preview || 'API Key (configured)';
      document.getElementById('okx-secret-key').placeholder = 'Secret Key (configured)';
      document.getElementById('okx-passphrase').placeholder = 'Passphrase (configured)';
    }
  } catch (e) { /* ignore */ }
}

async function saveOkxKeys() {
  const apiKey = document.getElementById('okx-api-key').value.trim();
  const secretKey = document.getElementById('okx-secret-key').value.trim();
  const passphrase = document.getElementById('okx-passphrase').value.trim();
  const result = document.getElementById('okx-result');
  result.className = 'register-result';
  result.style.display = 'none';

  if (!apiKey || !secretKey || !passphrase) {
    showResult(result, 'error', 'All three fields are required.');
    return;
  }

  const btn = document.getElementById('btn-okx-save');
  btn.disabled = true;
  btn.textContent = 'Saving...';

  try {
    const resp = await fetch('/api/okx/connect', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ api_key: apiKey, secret_key: secretKey, passphrase }),
    });
    const data = await resp.json();

    if (data.error) {
      showResult(result, 'error', data.error);
    } else if (data.warning) {
      showResult(result, 'error', data.message + ' — ' + data.warning);
      setOkxStatus(true, data.api_key_prefix);
    } else {
      showResult(result, 'success', data.message + ' (' + data.assets + ' assets found)');
      setOkxStatus(true, data.api_key_prefix);
      document.getElementById('okx-api-key').value = '';
      document.getElementById('okx-secret-key').value = '';
      document.getElementById('okx-passphrase').value = '';
      document.getElementById('okx-api-key').placeholder = data.api_key_prefix || 'API Key (configured)';
      document.getElementById('okx-secret-key').placeholder = 'Secret Key (configured)';
      document.getElementById('okx-passphrase').placeholder = 'Passphrase (configured)';
      loadTrades();
    }
  } catch (e) {
    showResult(result, 'error', 'Error: ' + (e.message || e));
  }
  btn.disabled = false;
  btn.textContent = 'Save';
}

function setOkxStatus(live, preview) {
  const el = document.getElementById('okx-status');
  if (live) {
    el.className = 'okx-status live';
    el.textContent = 'Live' + (preview ? ' (' + preview + ')' : '');
  } else {
    el.className = 'okx-status sim';
    el.textContent = 'Simulated';
  }
}

// ---------- OKX Trade History ----------

async function loadTrades() {
  try {
    const resp = await fetch('/api/okx/trades');
    const data = await resp.json();
    if (data.simulated) {
      document.getElementById('trade-sim-badge').style.display = 'inline';
    } else {
      document.getElementById('trade-sim-badge').style.display = 'none';
    }
    renderTrades(data.trades || []);
  } catch (e) {
    document.getElementById('trades-panel').innerHTML = '<div class="empty">Failed to load trades</div>';
  }
}

function renderTrades(trades) {
  const el = document.getElementById('trades-panel');
  if (!trades.length) { el.innerHTML = '<div class="empty">No trades found</div>'; return; }
  el.innerHTML = '<table><tr><th>Time</th><th>Pair</th><th>Side</th><th>Size</th><th>Price</th><th>PnL</th><th>Status</th></tr>' +
    trades.map(t => {
      const time = new Date(t.timestamp_ms).toLocaleTimeString();
      const cls = t.side === 'buy' ? 'trade-side-buy' : 'trade-side-sell';
      const pnl = t.pnl !== 0 ? (t.pnl > 0 ? '+' : '') + t.pnl.toFixed(4) : '-';
      const pnlCls = t.pnl > 0 ? 'color:var(--green)' : t.pnl < 0 ? 'color:var(--red)' : '';
      return '<tr>' +
        '<td>' + time + '</td>' +
        '<td><strong>' + t.inst_id + '</strong></td>' +
        '<td class="' + cls + '">' + t.side.toUpperCase() + '</td>' +
        '<td>' + t.size + '</td>' +
        '<td>$' + t.price.toLocaleString() + '</td>' +
        '<td style="' + pnlCls + '">' + pnl + '</td>' +
        '<td>' + t.state + '</td>' +
      '</tr>';
    }).join('') + '</table>';
}

// ---------- WebSocket ----------

function connect() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(proto + '//' + location.host + '/ws');
  ws.onopen = () => {
    document.getElementById('ws-status').classList.add('connected');
    document.getElementById('ws-label').textContent = 'Connected';
    fetchSnapshot();
  };
  ws.onclose = () => {
    document.getElementById('ws-status').classList.remove('connected');
    document.getElementById('ws-label').textContent = 'Disconnected';
    setTimeout(connect, 3000);
  };
  ws.onmessage = (e) => {
    try { handleEvent(JSON.parse(e.data)); } catch (err) { console.error('Parse error:', err); }
  };
}

async function fetchSnapshot() {
  try {
    const r = await fetch('/api/snapshot');
    const snap = await r.json();
    renderAgents(snap.agents || []);
    renderPending(snap.pending_proposals || []);
    renderX402(snap.pending_x402_payments || []);
    renderCredits(snap.active_credit_lines || []);
    renderPortfolio(snap.portfolio || {});
  } catch (e) { console.error('Snapshot fetch failed:', e); }
}

function handleEvent(ev) {
  addEventItem(ev);
  switch (ev.type) {
    case 'AgentRegistered': fetchSnapshot(); break;
    case 'CreditProposalPending': fetchSnapshot(); break;
    case 'CreditApproved': fetchSnapshot(); break;
    case 'CreditRejectedByHuman': fetchSnapshot(); break;
    case 'CreditRecalled': state.recalls++; fetchSnapshot(); break;
    case 'CreditRepaid': fetchSnapshot(); break;
    case 'BudgetUpdate': fetchSnapshot(); break;
    case 'TradeExecuted': state.trades++; state.approved++; break;
    case 'TradeRejected': state.trades++; state.rejected++; break;
    case 'X402PaymentPending': fetchSnapshot(); break;
    case 'X402PaymentApproved': fetchSnapshot(); break;
    case 'X402PaymentBlocked': fetchSnapshot(); break;
    case 'PortfolioUpdate': renderPortfolio(ev.balances || {}); break;
  }
  updateStats();
}

function addEventItem(ev) {
  const list = document.getElementById('event-list');
  if (list.querySelector('.empty')) list.innerHTML = '';
  const item = document.createElement('div');
  item.className = 'event-item';
  const time = new Date().toLocaleTimeString();
  let badge = '';
  if (ev.type === 'TradeExecuted') badge = '<span class="badge approved">executed</span>';
  else if (ev.type === 'TradeRejected') badge = '<span class="badge rejected">rejected</span>';
  else if (ev.type === 'CreditApproved') badge = '<span class="badge approved">credit approved</span>';
  else if (ev.type === 'CreditProposalPending') badge = '<span class="badge pending">needs approval</span>';
  else if (ev.type === 'CreditRejectedByHuman') badge = '<span class="badge rejected">credit rejected</span>';
  else if (ev.type === 'CreditRecalled') badge = '<span class="badge recalled">recalled</span>';
  else badge = '<span class="badge active">' + ev.type + '</span>';
  item.innerHTML = '<span style="color:var(--muted)">' + time + '</span> ' + badge + ' ' + summarize(ev);
  list.prepend(item);
  while (list.children.length > 50) list.lastChild.remove();
}

function summarize(ev) {
  if (ev.type === 'TradeExecuted') return ev.pair + ' ' + ev.side + ' $' + (ev.amount_usd||0).toFixed(2);
  if (ev.type === 'TradeRejected') return (ev.reason||'').substring(0, 80);
  if (ev.type === 'CreditApproved') return 'Agent ' + (ev.credit_line?.agent_id||'').substring(0,8) + ' $' + (ev.credit_line?.approved_usd||0).toFixed(2);
  if (ev.type === 'CreditProposalPending') return 'Agent ' + (ev.proposal?.agent_id||'').substring(0,8) + ' requests $' + (ev.proposal?.requested_usd||0).toFixed(2) + ' (score: ' + (ev.score||0).toFixed(1) + ')';
  if (ev.type === 'CreditRejectedByHuman') return 'Agent ' + (ev.agent_id||'').substring(0,8);
  if (ev.type === 'CreditRecalled') return 'Agent ' + (ev.agent_id||'').substring(0,8) + ' ' + (ev.reason||'');
  if (ev.type === 'AgentRegistered') return (ev.agent?.name||'unknown');
  if (ev.type === 'BudgetUpdate') return 'Agent ' + (ev.agent_id||'').substring(0,8) + ' spent=$' + (ev.spent_usd||0).toFixed(2) + ' rem=$' + (ev.remaining_usd||0).toFixed(2);
  if (ev.type === 'X402PaymentPending') return 'Agent ' + (ev.payment?.agent_id||'').substring(0,8) + ' $' + (ev.payment?.amount_usd||0).toFixed(2) + ' to ' + (ev.payment?.recipient||'').substring(0,12) + ' [' + (ev.risk||'') + ']';
  if (ev.type === 'X402PaymentApproved') return 'Payment ' + (ev.payment_id||'').substring(0,8) + ' approved';
  if (ev.type === 'X402PaymentBlocked') return 'Payment ' + (ev.payment_id||'').substring(0,8) + ' blocked: ' + (ev.reason||'');
  return JSON.stringify(ev).substring(0, 100);
}

function renderAgents(agents) {
  const el = document.getElementById('agents-panel');
  if (!agents.length) { el.innerHTML = '<div class="empty">No agents registered</div>'; return; }
  el.innerHTML = '<table><tr><th>ID</th><th>Name</th><th>Wallet</th><th>Registered</th></tr>' +
    agents.map(a => '<tr><td>' + a.id.substring(0,8) + '</td><td>' + a.name + '</td><td style="font-family:Consolas,monospace;font-size:12px">' + (a.evm_address ? a.evm_address.substring(0,6) + '...' + a.evm_address.substring(38) : '<span style="color:var(--muted)">none</span>') + '</td><td>' + new Date(a.registered_at).toLocaleTimeString() + '</td></tr>').join('') + '</table>';
}

function renderPending(proposals) {
  const el = document.getElementById('pending-panel');
  if (!proposals.length) { el.innerHTML = '<div class="empty">No pending proposals</div>'; return; }
  el.innerHTML = proposals.map(p => {
    const s = p.score;
    const cls = s >= 7 ? 'high' : s >= 5 ? 'mid' : 'low';
    const pid = p.proposal.id;
    return '<div class="proposal-card">' +
      '<div class="score ' + cls + '">' + s.toFixed(1) + '</div>' +
      '<strong>Agent ' + p.proposal.agent_id.substring(0,8) + '</strong> ' +
      '<span class="badge pending">pending</span>' +
      '<div class="meta">' +
        'Requested: <strong>$' + p.proposal.requested_usd.toFixed(2) + '</strong> | ' +
        'Recommended: <strong>$' + p.recommended_usd.toFixed(2) + '</strong> | ' +
        'Strategy: ' + (p.proposal.strategy||'').substring(0,60) + '<br>' +
        'Pairs: ' + (p.proposal.allowed_pairs||[]).join(', ') + ' | ' +
        'Max loss: $' + (p.proposal.max_loss_usd||0).toFixed(2) + ' | ' +
        'Max single: $' + (p.proposal.max_single_trade_usd||0).toFixed(2) +
      '</div>' +
      '<div class="actions">' +
        '<button class="btn btn-approve" onclick="approveCredit(\'' + pid + '\')">Approve ($' + p.recommended_usd.toFixed(2) + ')</button>' +
        '<button class="btn btn-reject" onclick="rejectCredit(\'' + pid + '\')">Reject</button>' +
      '</div>' +
    '</div>';
  }).join('');
}

async function approveCredit(proposalId) {
  const btns = document.querySelectorAll('.btn');
  btns.forEach(b => b.disabled = true);
  try {
    const r = await fetch('/api/credit/' + proposalId + '/approve', { method: 'POST' });
    const j = await r.json();
    if (j.ok) { fetchSnapshot(); }
    else { alert('Approve failed: ' + (j.error||'unknown')); }
  } catch(e) { alert('Error: ' + e); }
  btns.forEach(b => b.disabled = false);
}

async function rejectCredit(proposalId) {
  const btns = document.querySelectorAll('.btn');
  btns.forEach(b => b.disabled = true);
  try {
    const r = await fetch('/api/credit/' + proposalId + '/reject', { method: 'POST' });
    const j = await r.json();
    if (j.ok) { fetchSnapshot(); }
    else { alert('Reject failed: ' + (j.error||'unknown')); }
  } catch(e) { alert('Error: ' + e); }
  btns.forEach(b => b.disabled = false);
}

function renderX402(payments) {
  const el = document.getElementById('x402-panel');
  if (!payments.length) { el.innerHTML = '<div class="empty">No pending x402 payments</div>'; return; }
  el.innerHTML = payments.map(p => {
    const risk = p.risk_level;
    const cls = risk === 'High' ? 'low' : risk === 'Medium' ? 'mid' : 'high';
    const badge_cls = risk === 'High' ? 'rejected' : risk === 'Medium' ? 'pending' : 'approved';
    const pid = p.payment.id;
    return '<div class="proposal-card">' +
      '<div class="score ' + cls + '">' + risk + '</div>' +
      '<strong>Agent ' + p.payment.agent_id.substring(0,8) + '</strong> ' +
      '<span class="badge ' + badge_cls + '">x402 ' + risk + ' risk</span>' +
      '<div class="meta">' +
        'Recipient: <strong>' + p.payment.recipient + '</strong><br>' +
        'Amount: <strong>$' + p.payment.amount_usd.toFixed(2) + ' ' + p.payment.currency + '</strong> | ' +
        'Service: ' + (p.payment.service_url||'') + '<br>' +
        'Purpose: ' + (p.payment.purpose||'') + '<br>' +
        'Reason: <em>' + (p.reason||'') + '</em>' +
      '</div>' +
      '<div class="actions">' +
        '<button class="btn btn-approve" onclick="approveX402(\'' + pid + '\')">Approve Payment</button>' +
        '<button class="btn btn-reject" onclick="blockX402(\'' + pid + '\')">Block Payment</button>' +
      '</div>' +
    '</div>';
  }).join('');
}

async function approveX402(paymentId) {
  const btns = document.querySelectorAll('.btn');
  btns.forEach(b => b.disabled = true);
  try {
    const r = await fetch('/api/x402/' + paymentId + '/approve', { method: 'POST' });
    const j = await r.json();
    if (j.ok) { fetchSnapshot(); }
    else { alert('Approve failed: ' + (j.error||'unknown')); }
  } catch(e) { alert('Error: ' + e); }
  btns.forEach(b => b.disabled = false);
}

async function blockX402(paymentId) {
  const btns = document.querySelectorAll('.btn');
  btns.forEach(b => b.disabled = true);
  try {
    const r = await fetch('/api/x402/' + paymentId + '/block', { method: 'POST' });
    const j = await r.json();
    if (j.ok) { fetchSnapshot(); }
    else { alert('Block failed: ' + (j.error||'unknown')); }
  } catch(e) { alert('Error: ' + e); }
  btns.forEach(b => b.disabled = false);
}

function renderCredits(lines) {
  const el = document.getElementById('credit-panel');
  if (!lines.length) { el.innerHTML = '<div class="empty">No active credit lines</div>'; return; }
  el.innerHTML = lines.map(l => {
    const pct = l.approved_usd > 0 ? ((l.spent_usd / l.approved_usd) * 100) : 0;
    return '<div style="margin-bottom:12px"><strong>Agent ' + l.agent_id.substring(0,8) + '</strong> <span class="badge active">' + l.status + '</span>' +
      '<div style="color:var(--muted);font-size:12px">$' + l.spent_usd.toFixed(2) + ' / $' + l.approved_usd.toFixed(2) + '</div>' +
      '<div class="credit-bar"><div class="credit-bar-fill" style="width:' + pct + '%"></div></div></div>';
  }).join('');
}

function renderPortfolio(p) {
  const el = document.getElementById('portfolio-panel');
  const keys = Object.keys(p).sort();
  if (!keys.length) { el.innerHTML = '<div class="empty">No portfolio data</div>'; return; }
  el.innerHTML = '<table><tr><th>Asset</th><th>Balance</th></tr>' +
    keys.map(k => '<tr><td><strong>' + k + '</strong></td><td>' + Number(p[k]).toFixed(6) + '</td></tr>').join('') + '</table>';
}

function updateStats() {
  document.getElementById('stat-trades').textContent = state.trades;
  document.getElementById('stat-approved').textContent = state.approved;
  document.getElementById('stat-rejected').textContent = state.rejected;
  document.getElementById('stat-recalls').textContent = state.recalls;
}

connect();
loadOkxStatus();
loadTrades();
</script>
</body>
</html>"#;
