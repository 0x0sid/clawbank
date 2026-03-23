# openclaw-aibank — Build Bible

> AI agents propose. The Banker approves. The Guardian enforces. The Treasury holds the money.

---

## TODO

### Immediate (build these first)

- [x] `src/types.rs` — all types implemented: CreditProposal, CreditLine, CreditDecision, CreditStatus, RepaymentTrigger, Collateral, AgentReputation, DashboardEvent variants ✅ tested
- [x] `src/banker.rs` — credit line registry, deterministic scoring, force-recall, reputation tracking ✅ tested (10 unit + 7 integration)
- [x] `src/guardian.rs` — 6-check pipeline, check_credit_line is always first ✅ tested (10 unit + 7 integration)
- [x] `src/mcp/skill.rs` — 9 tools over JSON-RPC stdio (incl. `submit_x402_payment`), all handlers wired ✅ tested (10 unit + 7 integration)
- [x] `src/monitor.rs` — in-memory state store, portfolio + proposals ✅ builds, exercised by poller
- [x] `src/dashboard.rs` — Axum HTTP + WebSocket + approve/reject UI ✅ builds, serves on :3030, interactive credit approval tested live
- [x] `.github/workflows/ci.yml` — fmt → clippy → test → build → audit → deny → solhint ⚠️ not triggered yet (no PR opened)
- [x] `.github/workflows/security.yml` — weekly cargo audit + cargo deny ⚠️ not triggered yet (scheduled)
- [x] `.coderabbit.yaml` — AI review config with path-specific instructions ⚠️ not triggered yet (no PR opened)
- [x] `deny.toml` — license and supply chain policy ✅ builds

### Week 2 — OKX execution wiring

- [x] `src/execution/okx_cex.rs` — proxy to OKX Agent Trade Kit MCP subprocess ✅ builds ⚠️ simulated (okx-trade-mcp not installed)
- [x] `src/execution/okx_onchain.rs` — proxy to OKX OnchainOS skills subprocess ✅ builds ⚠️ simulated (onchainos-skills not installed)
- [x] `src/execution/okx_rest.rs` — real OKX REST client with HMAC-SHA256 signing ✅ builds ⚠️ NOT TESTED LIVE — OKX_PASSPHRASE missing in .env
- [x] Real portfolio poller replacing the stub in `main.rs` ✅ runs every 30s, falls back to simulated data if no creds
- [x] Force-cancel via OKX cancel-all-orders on credit recall ✅ implemented, wired to P&L position tracking ⚠️ not tested live
- [x] `.env` auto-loading via dotenvy ✅ builds, committed

### Week 3-4 — On-chain treasury

- [x] `contracts/AgentTreasury.sol` — ERC-4337 with validateUserOp credit enforcement ✅ written ✅ compiled with Foundry
- [x] `contracts/test/AgentTreasury.t.sol` — Foundry unit + fuzz tests ✅ written ✅ compiled with Foundry
- [ ] Deploy to Base Sepolia testnet (deploy script ready in `foundry-deploy/script/Deploy.s.sol`)
- [x] Wire Banker `grantCredit` / `recallCredit` to contract after `CreditDecision` ✅ TreasuryClient in `src/execution/treasury.rs` ✅ alloy crate for real tx signing on Base Sepolia

### Week 5-6 — Hardening

- [ ] Replace in-memory state with Redis
- [ ] TLS on dashboard
- [ ] Auth on dashboard (`Authorization: Bearer`)
- [ ] Prometheus `/metrics` endpoint
- [ ] Structured log export (Grafana / Loki)
- [ ] Load test guardian under concurrent proposal flood
- [ ] Fuzz guardian inputs with `cargo-fuzz`

### Backlog

- [ ] Multi-agent treasury sub-accounts on-chain
- [ ] Agent reputation ledger persisted on-chain
- [ ] Cross-agent collision detection (two agents on same pair simultaneously)
- [ ] Solidity audit (Foundry + Slither + manual review)

### x402 integration

- [x] `src/execution/x402.rs` — x402 payment interceptor + risk classifier ✅ 9 unit tests
- [x] `X402PaymentRequest` type in `types.rs` (recipient, amount, currency, purpose, service_url) ✅
- [x] `X402Verdict` and `X402RiskLevel` enums in `types.rs` ✅
- [x] Guardian hook: intercept x402 before payment signature is released ✅ via MCP `submit_x402_payment` tool
- [x] Dashboard: "Pending x402 Payments" panel with approve/block buttons ✅
- [x] Dashboard events: `X402PaymentPending`, `X402PaymentApproved`, `X402PaymentBlocked` ✅
- [x] Recipient blocklist / allowlist in `PolicyConfig` ✅
- [x] x402 payment deducts from credit line budget (same as buy trades) ✅
- [x] Tests: known recipient auto-approve, unknown flags, blocklisted auto-blocks ✅ 9 unit + 9 integration

### Known issues

- ✅ OKX API key has **Read + Trade** permission — live execution enabled
- ✅ `OKX_PASSPHRASE` configured in `.env` — HMAC signing works
- ✅ Dashboard credit approval/rejection tested live with real OKX trades
- ✅ $1 trade cap enforced at PolicyConfig, Guardian, and MCP handler levels
- ✅ Sell trades (reconversion to USDT) do not consume budget
- ⚠️ `okx-trade-mcp` and `onchainos-skills` not installed — falls back to OKX REST API
- ⚠️ CI workflows haven't been triggered yet — need to open a PR or push to trigger
- ✅ Solidity contract compiled with Foundry — deploy script ready
- ⚠️ AgentTreasury not yet deployed — needs BANKER_KEY with Base Sepolia ETH

---

## Table of Contents

1. [One-liner](#one-liner)
2. [Architecture](#architecture)
3. [The borrowing flow](#the-borrowing-flow)
4. [Component breakdown](#component-breakdown)
5. [Type system](#type-system)
6. [MCP tool manifest](#mcp-tool-manifest)
7. [On-chain treasury contract](#on-chain-treasury-contract)
8. [OKX integration layer](#okx-integration-layer)
9. [Code review and CI](#code-review-and-ci)
10. [Running locally](#running-locally)
11. [Environment variables](#environment-variables)
12. [Key design decisions](#key-design-decisions)

---

## One-liner

A supervised agentic trading system where OpenClaw AI agents borrow from a treasury, execute trades through OKX via a guardian-enforced MCP skill, and are monitored live on a Rust-served dashboard — with forced position recall if they exceed their approved credit line.

---

## Architecture

```
OpenClaw Agent(s)
      |
      | MCP stdio (JSON-RPC 2.0)
      v
+------------------------------------------+
|        openclaw-aibank (Rust binary)      |
|                                          |
|  [1] MCP Skill         src/mcp/skill.rs  |
|  [2] Banker            src/banker.rs     |
|  [3] Guardian          src/guardian.rs   |
|  [4] Monitor           src/monitor.rs    |
|  [5] Dashboard         src/dashboard.rs  |
|  [6] broadcast::channel<DashboardEvent>  |
+------------------------------------------+
      |                        |
      | if approved            | OKX poller
      v                        v
OKX Agent Trade Kit MCP    OKX OnchainOS Skills MCP
(CEX: spot, perps,         (DeFi: DEX swap, bridge,
 options, grid bots)        contracts, broadcasting)
      |                        |
      v                        v
  OKX Exchange API      OKX Onchain Gateway
  HMAC signed           simulate -> broadcast -> track

On-chain (EVM / ERC-4337):
+-------------------------------+
|   AgentTreasury.sol           |
|   validateUserOp:             |
|   - Banker co-signature check |
|   - Credit ceiling check      |
|   - Time window check         |
|   - Cumulative spend check    |
+-------------------------------+

x402 payment interception:
+-------------------------------+
|   x402 Handler (guardian)     |
|   Agent sends HTTP request    |
|   → Server returns 402        |
|   → Guardian intercepts:      |
|     - Recipient address check |
|     - Amount within policy?   |
|     - Purpose legitimate?     |
|   → Dashboard flags suspect   |
|   → Human approves/blocks     |
|   → Payment signature released|
+-------------------------------+
```

**Why two OKX layers?**

OKX shipped two MCP toolkits in March 2026:
- **Agent Trade Kit** — CEX: spot, perps, options, grid bots, algo orders
- **OnchainOS Skills** — DeFi: DEX swap across 500+ DEXs, cross-chain bridge, contract calls, broadcasting

We proxy to both. Keys never touch our code. OKX handles signing.

---

## The borrowing flow

### Step 1 — Register

Agent calls `agent_register` on startup. No trades possible without registration.

### Step 2 — Request credit

Agent calls `request_credit` with a full `CreditProposal`:

| Field | Type | Description |
|---|---|---|
| `requested_usd` | f64 | How much to borrow |
| `window_start` | DateTime | Trading window start |
| `window_end` | DateTime | Hard expiry — positions force-closed after |
| `strategy` | String | Plain English: what the agent will do |
| `allowed_pairs` | Vec\<String\> | Which pairs it will trade |
| `max_single_trade_usd` | f64 | Self-declared per-trade limit |
| `max_loss_usd` | f64 | Stop-loss: Banker recalls at this loss |
| `target_return_pct` | f64 | Expected return (used in scoring) |
| `repayment_trigger` | enum | When funds are returned |
| `collateral` | Option | What the agent stakes to back the request |

### Step 3 — Banker scores and queues for human approval

Deterministic scoring model (not AI):

```
score = (
  strategy_clarity  * 0.30   // specific and coherent?
  risk_return_ratio * 0.25   // target return realistic vs stop-loss?
  agent_reputation  * 0.30   // prior credit line performance
  collateral_quality* 0.15   // collateral quality if provided
)

Recommended amount based on score:
  score < 5.0  → 25% of requested
  score < 6.0  → 50% of requested
  score < 7.0  → 75% of requested
  score >= 7.0 → 100% of requested
```

Proposal is queued as **pending** with the recommended amount. A human must
approve or reject on the dashboard (`POST /api/credit/:id/approve` or `/reject`).

On human approval: `CreditLine` created in registry + `grantCredit()` called on contract.

### Step 4 — Agent proposes trades

Guardian checks in order (all must pass):

```
1. check_credit_line     active line? pair allowed? amount within budget? time in window?
2. check_policy          global pair whitelist? global per-trade limit?
3. check_confidence      agent confidence >= 40%?
4. check_rate_limit      under N trades/hour?
5. check_contract_safety for DeFi: contract whitelisted? method safe?
6. check_anomaly         suspicious proposal rate? escalating risk scores?
```

If approved: amount deducted from `remaining_usd` (buys only — sells return
capital to USDT and do not consume budget). Trade forwarded to OKX MCP.

**$1 hard cap per trade** — enforced at three levels:
1. `PolicyConfig.max_single_trade_usd = 1.0`
2. Guardian `check_policy` rejects trades > $1
3. MCP handler clamps `amount_usd` to `.min(1.0)`

### Step 5 — Monitor watches P&L

OKX poller every 30s. If `loss >= max_loss_usd`:

```
FORCE RECALL:
1. OKX cancel-all-orders
2. CreditLine.status = Recalled
3. Dashboard RecallEvent broadcast
4. Block all future proposals until new credit line approved
```

### Step 5b — x402 payment interception

When an agent encounters an HTTP 402 (Payment Required) response from an
external service, the guardian intercepts the payment before signing:

```
1. Agent hits API → gets 402 with payment instructions
2. Guardian inspects x402 payment details:
   - Recipient address: known? on a blocklist? first-time?
   - Amount: within credit line budget? within policy cap?
   - Purpose: matches agent's declared strategy?
   - Service: is the API the agent claims to need?
3. Risk classification:
   ✅ LOW  — known recipient, small amount, matches strategy → auto-approve
   ⚠️ MED  — first-time recipient or unusual amount → dashboard alert
   🚫 HIGH — blocklisted address, exceeds budget, off-strategy → auto-block
4. Dashboard shows x402 payment alert with full context
5. Human approves → payment signature released to facilitator
   Human rejects → payment blocked, agent notified
```

This prevents agents from draining funds to unknown addresses under
the guise of "paying for an API service".

### Step 6 — Repay

Agent calls `repay_credit`. Line marked `Repaid`. Reputation updated positively.

---

## Component breakdown

### `src/banker.rs`

```rust
pub async fn evaluate(&self, proposal: &CreditProposal) -> CreditDecision
pub async fn approve_proposal(&self, proposal_id: Uuid) -> Result<CreditLine>
pub async fn reject_proposal(&self, proposal_id: Uuid) -> Result<()>
pub async fn get_pending_proposals(&self) -> Vec<PendingProposalInfo>
pub async fn get_active_line(&self, agent_id: Uuid) -> Option<CreditLine>
pub async fn deduct(&self, agent_id: Uuid, amount: f64) -> Result<()>
pub async fn recall(&self, agent_id: Uuid, reason: String) -> Result<()>
pub async fn repay(&self, agent_id: Uuid) -> Result<()>
pub async fn reputation(&self, agent_id: Uuid) -> AgentReputation
```

Registry: `RwLock<HashMap<Uuid, CreditLine>>`. Read-heavy, write-rare.
Pending proposals: `RwLock<HashMap<Uuid, PendingProposalInfo>>`.

### `src/guardian.rs`

6 checks. `check_credit_line` is always first. Read-only access to credit lines.
Returns `GuardianResult` with per-check audit log and composite risk score.

### `src/mcp/skill.rs`

8 tools over JSON-RPC stdio. Stdout reserved for protocol only.

### `src/monitor.rs`

In-memory state store. Production: back with Redis.

### `src/dashboard.rs`

Six Axum routes:
- `GET /` — inline HTML with interactive approve/reject UI, no build step
- `GET /ws` — WebSocket live event stream
- `GET /api/snapshot` — full state JSON (includes pending proposals)
- `POST /api/credit/:id/approve` — approve a pending credit proposal
- `POST /api/credit/:id/reject` — reject a pending credit proposal

### `src/execution/okx_cex.rs`

Proxy to `okx-trade-mcp` subprocess. Spot, perps, options, grid bots.

### `src/execution/okx_onchain.rs`

Proxy to `onchainos-skills`. Flow: get quote -> simulate -> co-sign -> broadcast -> track.

### `src/execution/x402.rs`

x402 payment interception and legitimacy screening. When an agent attempts
an HTTP 402 payment flow:

```rust
pub async fn intercept_x402(
    payment: &X402PaymentRequest,
    credit_line: &CreditLine,
    policy: &PolicyConfig,
) -> X402Verdict

pub fn classify_risk(payment: &X402PaymentRequest) -> X402RiskLevel
```

Risk classification:
- **Low** — known recipient, amount ≤ $1, matches declared strategy → auto-approve
- **Medium** — first-time recipient, unusual amount → flag on dashboard for human review
- **High** — blocklisted address, exceeds budget, off-strategy → auto-block

Dashboard events: `X402PaymentPending`, `X402PaymentApproved`, `X402PaymentBlocked`.

---

## Type system

```rust
pub struct CreditProposal {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub submitted_at: DateTime<Utc>,
    pub requested_usd: f64,
    pub max_loss_usd: f64,
    pub target_return_pct: f64,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub strategy: String,
    pub allowed_pairs: Vec<String>,
    pub max_single_trade_usd: f64,
    pub repayment_trigger: RepaymentTrigger,
    pub collateral: Option<Collateral>,
}

pub enum RepaymentTrigger {
    ProfitTarget { pct: f64 },
    StopLoss { loss_usd: f64 },
    TimeExpiry,
    Manual,
}

pub struct Collateral {
    pub asset: String,
    pub amount: f64,
    pub locked_at: DateTime<Utc>,
}

pub struct CreditLine {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub agent_id: Uuid,
    pub approved_usd: f64,
    pub spent_usd: f64,
    pub remaining_usd: f64,
    pub status: CreditStatus,
    pub approved_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub conditions: ApprovedConditions,
    pub reputation_at_approval: f64,
}

pub enum CreditStatus { Active, Suspended, Recalled, Expired, Repaid }

pub struct ApprovedConditions {
    pub allowed_pairs: Vec<String>,
    pub max_single_trade_usd: f64,
    pub max_loss_usd: f64,
    pub window_end: DateTime<Utc>,
}

pub struct CreditDecision {
    pub proposal_id: Uuid,
    pub approved: bool,
    pub approved_usd: Option<f64>,
    pub rejection_reason: Option<String>,
    pub score: f64,
    pub credit_line: Option<CreditLine>,
}

pub struct AgentReputation {
    pub agent_id: Uuid,
    pub score: f64,
    pub lines_approved: u32,
    pub lines_repaid_cleanly: u32,
    pub lines_recalled: u32,
    pub avg_utilization_pct: f64,
    pub avg_return_pct: f64,
}
```

New `DashboardEvent` variants:

```rust
CreditProposalPending { proposal, score, recommended_usd },
CreditApproved(CreditLine),
CreditRejectedByHuman { proposal_id, agent_id },
CreditRecalled { agent_id: Uuid, reason: String },
CreditRepaid { agent_id: Uuid },
BudgetUpdate { agent_id: Uuid, spent_usd: f64, remaining_usd: f64 },
X402PaymentPending { payment: X402PaymentRequest, risk: X402RiskLevel },
X402PaymentApproved { payment_id: Uuid, agent_id: Uuid },
X402PaymentBlocked { payment_id: Uuid, agent_id: Uuid, reason: String },
```

---

## MCP tool manifest

| Tool | Needs credit line | Description |
|---|---|---|
| `agent_register` | No | Register on startup |
| `request_credit` | No | Submit `CreditProposal` to Banker |
| `propose_trade` | Yes | Submit trade proposal |
| `repay_credit` | Yes | Signal repayment, close line |
| `get_portfolio` | No | Read portfolio state |
| `list_proposals` | No | Recent proposals with guardian results |
| `get_risk_score` | No | Current rolling risk score |
| `get_credit_line` | No | Read active credit line state |

---

## On-chain treasury contract

`contracts/AgentTreasury.sol` — ERC-4337 on Base.

`validateUserOp` enforces: Banker co-sig + credit ceiling + time window + cumulative spend.

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "@account-abstraction/contracts/core/BaseAccount.sol";

contract AgentTreasury is BaseAccount {
    address public banker;
    IEntryPoint private immutable _entryPoint;

    mapping(address => uint256) public creditCeiling;
    mapping(address => uint256) public creditSpent;
    mapping(address => uint256) public creditExpiry;

    event CreditGranted(address agent, uint256 ceiling, uint256 expiry);
    event CreditRecalled(address agent, string reason);

    modifier onlyBanker() { require(msg.sender == banker, "not banker"); _; }

    function grantCredit(address agent, uint256 ceiling, uint256 expiry)
        external onlyBanker
    {
        creditCeiling[agent] = ceiling;
        creditSpent[agent]   = 0;
        creditExpiry[agent]  = expiry;
        emit CreditGranted(agent, ceiling, expiry);
    }

    function recallCredit(address agent, string calldata reason)
        external onlyBanker
    {
        creditCeiling[agent] = 0;
        emit CreditRecalled(agent, reason);
    }

    function _validateSignature(
        PackedUserOperation calldata userOp,
        bytes32 userOpHash
    ) internal override returns (uint256) {
        (, bytes memory bankerSig) = abi.decode(userOp.signature, (bytes, bytes));
        if (_recoverSigner(userOpHash, bankerSig) != banker)
            return SIG_VALIDATION_FAILED;

        address agent  = userOp.sender;
        uint256 amount = _parseAmount(userOp.callData);

        if (block.timestamp > creditExpiry[agent])                   return SIG_VALIDATION_FAILED;
        if (creditSpent[agent] + amount > creditCeiling[agent])      return SIG_VALIDATION_FAILED;

        creditSpent[agent] += amount;
        return SIG_VALIDATION_SUCCESS;
    }

    function entryPoint() public view override returns (IEntryPoint) { return _entryPoint; }
}
```

Deploy: Base Sepolia (testnet) then Base mainnet. USDC funded treasury.
Banker key: env var only, never in agent runtime.

---

## Code review and CI

### Tool stack — $0 total for open source

| Tool | Cost | Purpose |
|---|---|---|
| `cargo fmt` | Free | Format enforcement |
| `cargo clippy -- -D warnings` | Free | Lint, catches logic errors |
| `cargo audit` | Free | CVE check on dependencies |
| `cargo deny` | Free | License + supply chain |
| `solhint` | Free | Solidity lint |
| GitHub Actions | Free (public repos) | CI runner |
| CodeRabbit | Free (open source) | AI PR review |
| Branch protection | Free | Enforces gates before merge |

### `.github/workflows/ci.yml`

```yaml
name: CI
on:
  push:
    branches: [main, dev]
  pull_request:
    branches: [main]
env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-Dwarnings"
jobs:
  fmt:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt }
      - run: cargo fmt --all -- --check
  clippy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: clippy }
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --all-targets --all-features -- -D warnings
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --all-features
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo build --release
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-audit --locked
      - run: cargo audit
  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v1
  solidity:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 20 }
      - run: npm install -g solhint && solhint 'contracts/**/*.sol'
```

### `.github/workflows/security.yml`

```yaml
name: Security scan
on:
  schedule:
    - cron: '0 6 * * 1'
  workflow_dispatch:
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-audit --locked
      - run: cargo audit --deny warnings
  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v1
        with:
          command: check advisories licenses sources
```

### `.coderabbit.yaml`

```yaml
language: en-US
reviews:
  profile: assertive
  request_changes_workflow: true
  high_level_summary: true
  poem: false
  path_instructions:
    - path: "src/guardian.rs"
      instructions: >
        Safety-critical financial code. Flag any bypass path around
        check_credit_line. Eliminate all unwrap()/expect(). Check every
        arithmetic operation for overflow. Verify check order is unchanged.
    - path: "src/banker.rs"
      instructions: >
        Check scoring formula edge cases. Verify force-recall is atomic.
        No race condition between reading and writing remaining_usd.
        Verify RwLock usage has no deadlocks.
    - path: "contracts/*.sol"
      instructions: >
        ERC-4337 treasury. Check validateUserOp for bypass paths. Verify
        signature recovery. Check reentrancy on withdrawals. Verify 0.8+
        overflow protection is active.
    - path: "src/mcp/skill.rs"
      instructions: >
        stdout must never be contaminated with logs. Check JSON-RPC parsing
        does not panic on malformed input. Verify all handlers return correct
        MCP error codes on failure.
  auto_review:
    enabled: true
    drafts: false
    base_branches: [main, dev]
```

### `deny.toml`

```toml
[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "ISC", "Unicode-DFS-2016"]
deny  = ["GPL-2.0", "GPL-3.0", "AGPL-3.0"]
[advisories]
ignore = []
[bans]
multiple-versions = "warn"
wildcards = "deny"
[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

### Local pre-commit hook

```bash
#!/bin/sh
set -e
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
echo "Pre-commit passed."
```

Save to `.git/hooks/pre-commit`, then `chmod +x .git/hooks/pre-commit`.

### Branch protection (GitHub settings, main branch)

Required status checks before merge: `fmt`, `clippy`, `test`, `build`, `audit`, `deny`
Require 1 approval. Dismiss stale reviews. No force pushes.

---

## Running locally

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# OKX Agent Trade Kit (CEX)
npm install -g @okx_ai/okx-trade-mcp @okx_ai/okx-trade-cli
okx config init

# OKX OnchainOS (DeFi)
curl -sSL https://raw.githubusercontent.com/okx/onchainos-skills/main/install.sh | sh

# Build and run
git clone https://github.com/0x0sid/openclaw-aibank && cd openclaw-aibank
cargo build --release

DASHBOARD_PORT=3030 OKX_API_KEY=key OKX_SECRET_KEY=secret OKX_PASSPHRASE=pass \
  ./target/release/openclaw-aibank
```

Dashboard at `http://localhost:3030`.

Register in OpenClaw (`~/.openclaw/config.yaml`):

```yaml
skills:
  - name: ai-bank
    command: /path/to/openclaw-aibank
    transport: stdio
```

---

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
| `MAX_CREDIT_USD` | No | unlimited | Global borrow ceiling per agent |
| `RECALL_CHECK_INTERVAL_SECS` | No | 30 | P&L check frequency |

---

## Key design decisions

**Off-chain Banker + on-chain enforcement, not just one?**
Off-chain catches 99% of issues in microseconds, tracks reputation, scores proposals. On-chain is the cryptographic last resort — even if the Banker is compromised, the contract won't release more than approved. Both layers are needed.

**Proxy to OKX MCP servers, not our own client?**
OKX shipped Agent Trade Kit and OnchainOS in March 2026. They handle signing, credential isolation, rate limiting. We focus on the guardian and banker — the layer OKX didn't build. No credential code in our repo means no credential leaks.

**Rust, not Python or TypeScript?**
MCP skill is on the critical path of every trade. Tokio handles concurrent agents with no GIL. The type system makes it impossible to pass a proposal to execution without going through the guardian.

**ERC-4337 for the treasury contract?**
`validateUserOp` is a standard enforcement hook before every withdrawal. Enforces ceilings, co-signatures, and time windows at contract level. Production-ready, audited, supported by bundlers on all major L2s.

**stdout reserved for MCP?**
OpenClaw reads stdout as JSON-RPC. One `println!` corrupts the protocol and crashes the skill. All `tracing` output goes to stderr. Non-negotiable.

**CodeRabbit for AI review?**
Free for open source, 2-minute GitHub setup, path-specific review instructions. Self-hosted Ollama + PR-Agent is the right call once funded.
