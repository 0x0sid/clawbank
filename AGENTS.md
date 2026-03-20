# openclaw-aibank

Supervised agentic trading system. OpenClaw agents borrow from a treasury,
trade via OKX, and are monitored by a guardian. Written in Rust.

> Full architecture and design decisions: see BUILD_BIBLE.md

---

## What this repo does

An MCP skill (JSON-RPC over stdio) that sits between OpenClaw agents and OKX.
Agents must request a credit line before trading. Every trade proposal runs
through a guardian before reaching OKX. A live WebSocket dashboard at :3030
shows all activity in real time.

Three concurrent tokio tasks in one binary:
1. MCP stdio loop (stdin/stdout — never pollute stdout with logs)
2. Axum dashboard server (:3030)
3. OKX portfolio poller (every 30s)

---

## Commands

```bash
# Build
cargo build --release

# Run (set env vars first — see Environment below)
./target/release/openclaw-aibank

# Tests
cargo test --all-features

# Lint (must pass before any commit)
cargo clippy --all-targets --all-features -- -D warnings

# Format check
cargo fmt --all -- --check

# Security audit
cargo audit

# Dependency license check
cargo deny check
```

## Environment variables

```
OKX_API_KEY          required   OKX CEX API key
OKX_SECRET_KEY       required   OKX secret key
OKX_PASSPHRASE       required   OKX passphrase
OKX_ONCHAIN_API_KEY  optional   OKX OnchainOS key (for DeFi)
BANKER_KEY           optional   Treasury co-signing private key
TREASURY_ADDRESS     optional   Deployed AgentTreasury contract address
DASHBOARD_PORT       optional   Dashboard port (default 3030)
RUST_LOG             optional   Log level (default info)
```

---

## Critical constraints

- **stdout is MCP protocol only.** All `tracing` output goes to stderr.
  Never use `println!` — use `tracing::info!` etc.
- **Guardian runs before every execution.** No trade reaches OKX without
  passing all 6 checks. Do not add bypass paths.
- **Credit line check is first.** `check_credit_line` must remain the
  first check in `guardian.rs`. Do not reorder.
- **Banker has write access to credit lines. Guardian has read only.**
  Never give the guardian write access to `CreditLine`.
- **No private keys in agent runtime.** OKX credentials live in
  `~/.okx/config` (managed by `okx config init`). Banker key in env var only.

---

## File structure

```
src/
  main.rs           entrypoint, spawns 3 tokio tasks
  types.rs          all shared types
  banker.rs         credit line registry, scoring, force-recall
  guardian.rs       6-check risk verification
  monitor.rs        in-memory state store
  dashboard.rs      Axum HTTP + WebSocket + inline HTML
  execution/
    okx_cex.rs      OKX Agent Trade Kit MCP proxy
    okx_onchain.rs  OKX OnchainOS skills proxy
  mcp/
    skill.rs        JSON-RPC over stdio
contracts/
  AgentTreasury.sol ERC-4337 treasury with credit enforcement
tests/
  guardian_tests.rs
  banker_tests.rs
  mcp_tests.rs
```

---

## Code style

- No `unwrap()` or `expect()` in production paths — use `?` and typed errors
- All public functions have doc comments
- Arithmetic on financial amounts uses `f64` — document precision assumptions
- New guardian checks go in `guardian.rs` only, follow the existing pattern
- New MCP tools go in `mcp/skill.rs` and must be added to `build_manifest()`

---

## Testing

- Unit tests live in the same file as the code (`#[cfg(test)]` module)
- Integration tests in `tests/`
- Guardian checks each have at least one passing and one failing test case
- Run `cargo test --all-features` before pushing

---

## CI

GitHub Actions runs on every push and PR:
`fmt` → `clippy` → `test` → `build` → `audit` → `deny`

All jobs must pass. PRs that fail CI are not merged.
CodeRabbit reviews every PR automatically — see `.coderabbit.yaml`.

---

## Key dependencies

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime |
| `axum` | HTTP + WebSocket dashboard |
| `serde` / `serde_json` | JSON serialization |
| `reqwest` | OKX REST client |
| `hmac` + `sha2` | OKX API signing |
| `uuid` | Proposal and credit line IDs |
| `chrono` | Timestamps and time windows |
| `tracing` | Structured logging (to stderr) |

---

## See also

- `BUILD_BIBLE.md` — full architecture, borrowing flow, contract spec, roadmap
- `CLAUDE.md` — points here (for Claude Code compatibility)
- `.coderabbit.yaml` — AI review config
- `.github/workflows/ci.yml` — CI pipeline
- `deny.toml` — license and dependency policy
