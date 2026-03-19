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
        State,
    },
    response::{Html, IntoResponse, Json},
    routing::get,
    Router,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info};

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
    let lines = state.banker.get_active_lines().await;
    let reps = state.banker.get_reputations().await;
    let snapshot = state.monitor.snapshot(agents, lines, reps).await;
    Json(snapshot)
}

/// Inline HTML for the dashboard — no build step required.
const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>OpenClaw AI Bank — Dashboard</title>
<style>
  :root {
    --bg: #0a0e17; --surface: #111827; --border: #1f2937;
    --text: #e5e7eb; --muted: #9ca3af; --accent: #3b82f6;
    --green: #10b981; --red: #ef4444; --yellow: #f59e0b;
    --font: 'Segoe UI', system-ui, -apple-system, sans-serif;
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    background: var(--bg); color: var(--text); font-family: var(--font);
    font-size: 14px; line-height: 1.5;
  }
  header {
    background: var(--surface); border-bottom: 1px solid var(--border);
    padding: 16px 24px; display: flex; align-items: center; gap: 16px;
  }
  header h1 { font-size: 18px; font-weight: 600; }
  .status-dot {
    width: 10px; height: 10px; border-radius: 50%;
    background: var(--red); display: inline-block;
  }
  .status-dot.connected { background: var(--green); }
  .grid {
    display: grid; grid-template-columns: 1fr 1fr;
    gap: 16px; padding: 24px; max-width: 1400px; margin: 0 auto;
  }
  .panel {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 16px; min-height: 200px;
  }
  .panel h2 {
    font-size: 13px; text-transform: uppercase; letter-spacing: 0.05em;
    color: var(--muted); margin-bottom: 12px; font-weight: 600;
  }
  .full-width { grid-column: 1 / -1; }
  .event-list { max-height: 400px; overflow-y: auto; }
  .event-item {
    padding: 8px 12px; border-bottom: 1px solid var(--border);
    font-size: 13px; font-family: 'Consolas', monospace;
  }
  .event-item:last-child { border-bottom: none; }
  .badge {
    display: inline-block; padding: 2px 8px; border-radius: 4px;
    font-size: 11px; font-weight: 600; text-transform: uppercase;
  }
  .badge.approved { background: rgba(16,185,129,0.2); color: var(--green); }
  .badge.rejected { background: rgba(239,68,68,0.2); color: var(--red); }
  .badge.active { background: rgba(59,130,246,0.2); color: var(--accent); }
  .badge.recalled { background: rgba(245,158,11,0.2); color: var(--yellow); }
  .credit-bar {
    height: 8px; border-radius: 4px; background: var(--border);
    margin-top: 8px; overflow: hidden;
  }
  .credit-bar-fill {
    height: 100%; border-radius: 4px; background: var(--accent);
    transition: width 0.3s ease;
  }
  .stat { text-align: center; padding: 12px; }
  .stat-value { font-size: 24px; font-weight: 700; color: var(--accent); }
  .stat-label { font-size: 11px; color: var(--muted); text-transform: uppercase; }
  .stats-row { display: flex; gap: 24px; flex-wrap: wrap; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th { text-align: left; color: var(--muted); font-weight: 600; padding: 8px; border-bottom: 1px solid var(--border); }
  td { padding: 8px; border-bottom: 1px solid var(--border); }
  .empty { color: var(--muted); font-style: italic; padding: 24px; text-align: center; }
</style>
</head>
<body>
<header>
  <h1>OpenClaw AI Bank</h1>
  <span class="status-dot" id="ws-status"></span>
  <span id="ws-label" style="color:var(--muted);font-size:12px;">Disconnected</span>
</header>

<div class="grid">
  <div class="panel">
    <h2>Agents</h2>
    <div id="agents-panel"><div class="empty">No agents registered</div></div>
  </div>

  <div class="panel">
    <h2>Credit Lines</h2>
    <div id="credit-panel"><div class="empty">No active credit lines</div></div>
  </div>

  <div class="panel">
    <h2>Portfolio</h2>
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
    try {
      const event = JSON.parse(e.data);
      handleEvent(event);
    } catch (err) { console.error('Parse error:', err); }
  };
}

async function fetchSnapshot() {
  try {
    const r = await fetch('/api/snapshot');
    const snap = await r.json();
    renderAgents(snap.agents || []);
    renderCredits(snap.active_credit_lines || []);
    renderPortfolio(snap.portfolio || {});
  } catch (e) { console.error('Snapshot fetch failed:', e); }
}

function handleEvent(ev) {
  addEventItem(ev);
  switch (ev.type) {
    case 'AgentRegistered': fetchSnapshot(); break;
    case 'CreditApproved': fetchSnapshot(); break;
    case 'CreditRecalled': state.recalls++; fetchSnapshot(); break;
    case 'CreditRepaid': fetchSnapshot(); break;
    case 'BudgetUpdate': fetchSnapshot(); break;
    case 'TradeExecuted': state.trades++; state.approved++; break;
    case 'TradeRejected': state.trades++; state.rejected++; break;
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
  else if (ev.type === 'CreditRecalled') badge = '<span class="badge recalled">recalled</span>';
  else badge = '<span class="badge active">' + ev.type + '</span>';
  item.innerHTML = '<span style="color:var(--muted)">' + time + '</span> ' + badge + ' ' + summarize(ev);
  list.prepend(item);
  while (list.children.length > 50) list.lastChild.remove();
}

function summarize(ev) {
  if (ev.type === 'TradeExecuted') return ev.pair + ' ' + ev.side + ' $' + (ev.amount_usd||0).toFixed(2);
  if (ev.type === 'TradeRejected') return (ev.reason||'').substring(0, 80);
  if (ev.type === 'CreditApproved') return 'Agent ' + (ev.credit_line?.agent_id||'').substring(0,8) + ' — $' + (ev.credit_line?.approved_usd||0).toFixed(2);
  if (ev.type === 'CreditRecalled') return 'Agent ' + (ev.agent_id||'').substring(0,8) + ' — ' + (ev.reason||'');
  if (ev.type === 'AgentRegistered') return (ev.agent?.name||'unknown');
  if (ev.type === 'BudgetUpdate') return 'Agent ' + (ev.agent_id||'').substring(0,8) + ' spent=$' + (ev.spent_usd||0).toFixed(2) + ' rem=$' + (ev.remaining_usd||0).toFixed(2);
  return JSON.stringify(ev).substring(0, 100);
}

function renderAgents(agents) {
  const el = document.getElementById('agents-panel');
  if (!agents.length) { el.innerHTML = '<div class="empty">No agents registered</div>'; return; }
  el.innerHTML = '<table><tr><th>ID</th><th>Name</th><th>Registered</th></tr>' +
    agents.map(a => '<tr><td>' + a.id.substring(0,8) + '…</td><td>' + a.name + '</td><td>' + new Date(a.registered_at).toLocaleString() + '</td></tr>').join('') + '</table>';
}

function renderCredits(lines) {
  const el = document.getElementById('credit-panel');
  if (!lines.length) { el.innerHTML = '<div class="empty">No active credit lines</div>'; return; }
  el.innerHTML = lines.map(l => {
    const pct = l.approved_usd > 0 ? ((l.spent_usd / l.approved_usd) * 100) : 0;
    return '<div style="margin-bottom:12px"><strong>Agent ' + l.agent_id.substring(0,8) + '</strong> <span class="badge active">' + l.status + '</span>' +
      '<div style="color:var(--muted);font-size:12px">$' + l.spent_usd.toFixed(2) + ' / $' + l.approved_usd.toFixed(2) + ' — expires ' + new Date(l.expires_at).toLocaleString() + '</div>' +
      '<div class="credit-bar"><div class="credit-bar-fill" style="width:' + pct + '%"></div></div></div>';
  }).join('');
}

function renderPortfolio(p) {
  const el = document.getElementById('portfolio-panel');
  const keys = Object.keys(p);
  if (!keys.length) { el.innerHTML = '<div class="empty">No portfolio data</div>'; return; }
  el.innerHTML = '<table><tr><th>Asset</th><th>Balance</th></tr>' +
    keys.map(k => '<tr><td>' + k + '</td><td>' + p[k].toFixed(4) + '</td></tr>').join('') + '</table>';
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
