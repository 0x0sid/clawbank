# openclaw-aibank

Supervised agentic trading system. OpenClaw agents borrow from a treasury,
trade via OKX, and are monitored by a guardian. Written in Rust.

## What this does

An MCP skill (JSON-RPC over stdio) that sits between OpenClaw agents and OKX.
Agents must request a credit line before trading. Every trade proposal runs
through a 6-check guardian before reaching OKX. A live WebSocket dashboard at `:3030`
shows all activity in real time.

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
| `DASHBOARD_PORT` | No | 3030 | Dashboard HTTP port |
| `RUST_LOG` | No | info | Log level |

## Architecture

```
OpenClaw Agent(s) → MCP stdio (JSON-RPC 2.0)
    → Banker (credit scoring, approval)
    → Guardian (6-check risk verification)
    → OKX Agent Trade Kit (CEX) / OKX OnchainOS (DeFi)
    → Dashboard (live WebSocket at :3030)
```

## MCP tools

| Tool | Description |
|---|---|
| `agent_register` | Register on startup |
| `request_credit` | Submit credit proposal to Banker |
| `propose_trade` | Submit trade for Guardian review + execution |
| `repay_credit` | Signal repayment, close credit line |
| `get_portfolio` | Read portfolio state |
| `list_proposals` | Recent proposals with guardian results |
| `get_risk_score` | Current agent reputation/risk score |
| `get_credit_line` | Read active credit line state |

## License

MIT
