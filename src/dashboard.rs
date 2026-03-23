//! Axum HTTP + WebSocket dashboard — no build step, inline HTML.
//!
//! Three routes:
//! - `GET /`            — inline HTML dashboard
//! - `GET /ws`          — WebSocket live event stream
//! - `GET /api/snapshot` — full state JSON

use crate::banker::Banker;
use crate::monitor::Monitor;
use crate::types::DashboardEvent;
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
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// Shared state for dashboard handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub banker: Arc<Banker>,
    pub monitor: Arc<Monitor>,
    pub tx: broadcast::Sender<DashboardEvent>,
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
</style>
</head>
<body>
<header>
  <h1>OpenClaw AI Bank</h1>
  <span class="status-dot" id="ws-status"></span>
  <span id="ws-label" style="color:var(--muted);font-size:12px;">Disconnected</span>
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
    <h2>Live Events</h2>
    <div class="event-list" id="event-list"><div class="empty">Waiting for events...</div></div>
  </div>
</div>

<script>
const state = { trades: 0, approved: 0, rejected: 0, recalls: 0 };
let ws;

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
  el.innerHTML = '<table><tr><th>ID</th><th>Name</th><th>Registered</th></tr>' +
    agents.map(a => '<tr><td>' + a.id.substring(0,8) + '</td><td>' + a.name + '</td><td>' + new Date(a.registered_at).toLocaleTimeString() + '</td></tr>').join('') + '</table>';
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
</script>
</body>
</html>"#;
