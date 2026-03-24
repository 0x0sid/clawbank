# openclaw-aibank

Supervised agentic trading system. OpenClaw agents borrow from a treasury,
trade via OKX, and are monitored by a guardian. Written in Rust.

## What this does

An MCP skill (JSON-RPC over stdio) that sits between OpenClaw agents and OKX.
Agents must request a credit line before trading. Credit proposals require
**human approval** on the dashboard before activation. Every trade proposal runs
through a 6-check guardian before reaching OKX. A live WebSocket dashboard at `:3030`
shows all activity in real time and allows interactive credit decisions.

Three concurrent tokio tasks in one binary:
1. **MCP stdio loop** — stdin/stdout JSON-RPC (never pollute stdout with logs)
2. **Axum dashboard server** — `:3030` HTTP + WebSocket + inline HTML
3. **OKX portfolio poller** — every 30s, checks P&L, triggers force-recall if needed

## Quick start

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build
cargo build --release

# Run (set env vars first)
DASHBOARD_PORT=3030 OKX_API_KEY=key OKX_SECRET_KEY=secret OKX_PASSPHRASE=pass \
  ./target/release/openclaw-aibank

# Tests
cargo test --all-features

# Lint
cargo clippy --all-targets --all-features -- -D warnings
```

## Environment variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `OKX_API_KEY` | Yes | | OKX CEX API key |
| `OKX_SECRET_KEY` | Yes | | OKX secret key |
| `OKX_PASSPHRASE` | Yes | | OKX passphrase |
| `OKX_ONCHAIN_API_KEY` | For DeFi | | OKX OnchainOS key |
| `BANKER_KEY` | For on-chain | | Treasury co-signing private key |
| `TREASURY_ADDRESS` | For on-chain | | Deployed contract address |
| `TREASURY_RPC_URL` | For on-chain | `https://sepolia.base.org` | EVM JSON-RPC endpoint |
| `DASHBOARD_PORT` | No | 3030 | Dashboard HTTP port |
| `RUST_LOG` | No | info | Log level |

## Architecture

```
OpenClaw Agent(s) → MCP stdio (JSON-RPC 2.0)
    → Banker (credit scoring, human approval queue)
    → Dashboard (approve/reject pending proposals at :3030)
    → Guardian (6-check risk verification, $1 hard cap)
    → OKX Agent Trade Kit (CEX) / OKX REST fallback
    → Dashboard (live WebSocket event stream)
```

**Key constraints:**
- **$1 hard cap per trade** — enforced at PolicyConfig, Guardian, and MCP handler
- **Sell trades do not consume budget** — reconverting to USDT is capital return
- **Human approval required** — credit proposals queue as pending until approved/rejected

## Simulation

```powershell
# Run the agent simulation (registers, requests $1 credit, buy+sell BTC)
powershell -ExecutionPolicy Bypass -File .\scripts\agent-sim.ps1
# Then open http://localhost:3030 and click Approve or Reject
```

## Dashboard features

- **Web3 Wallet Connect** — connect MetaMask/OKX Wallet/Rabby to register agents with EVM address + signature proof. Greyed out when no extension detected.
- **OKX Trade History** — live trade table from your OKX account (or simulated demo trades when no credentials). Shows pair, side, size, price, PnL, status.
- **Settings → OKX CEX API** — view connection status, replace API keys at runtime. Credentials loaded from `.env` by default. Bad keys are rejected and not saved.
- **Portfolio (OKX)** — real-time balances polled every 30s.
- **Credit Proposals** — approve/reject pending agent credit requests.
- **x402 Payments** — review flagged agent payment attempts.

## Dashboard API

| Endpoint | Method | Description |
|---|---|---|
| `/` | GET | Interactive dashboard with approve/reject UI |
| `/ws` | GET | WebSocket live event stream |
| `/api/snapshot` | GET | Full state JSON (includes pending proposals) |
| `/api/credit/:id/approve` | POST | Approve a pending credit proposal |
| `/api/credit/:id/reject` | POST | Reject a pending credit proposal |
| `/api/agent/register` | POST | Register agent via wallet signature verification |
| `/api/okx/connect` | POST | Save OKX API credentials at runtime |
| `/api/okx/status` | GET | OKX connection status + masked key preview |
| `/api/okx/trades` | GET | Recent OKX trade history (live or simulated) |

## MCP tools

| Tool | Description |
|---|---|
| `agent_register` | Register on startup |
| `request_credit` | Submit credit proposal to Banker (queued for human approval) |
| `propose_trade` | Submit trade for Guardian review + execution (max $1) |
| `repay_credit` | Signal repayment, close credit line |
| `get_portfolio` | Read portfolio state |
| `list_proposals` | Recent proposals with guardian results |
| `get_risk_score` | Current agent reputation/risk score |
| `get_credit_line` | Read active credit line state |

## License

MIT
