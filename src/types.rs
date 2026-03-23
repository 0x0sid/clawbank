//! All shared types for the openclaw-aibank system.
//!
//! This module defines the core domain types used across the banker, guardian,
//! monitor, dashboard, and MCP skill layers. Financial amounts use `f64` —
//! precision is sufficient for USD credit line tracking at the scale we operate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Agent registration
// ---------------------------------------------------------------------------

/// An agent that has registered with the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
    pub registered_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Credit system
// ---------------------------------------------------------------------------

/// A proposal submitted by an agent requesting a credit line.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// When the borrowed funds should be returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RepaymentTrigger {
    ProfitTarget { pct: f64 },
    StopLoss { loss_usd: f64 },
    TimeExpiry,
    Manual,
}

/// Collateral staked by an agent to back a credit request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collateral {
    pub asset: String,
    pub amount: f64,
    pub locked_at: DateTime<Utc>,
}

/// An approved credit line granted by the Banker.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Status of a credit line through its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreditStatus {
    Active,
    Suspended,
    Recalled,
    Expired,
    Repaid,
}

/// Conditions attached to an approved credit line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedConditions {
    pub allowed_pairs: Vec<String>,
    pub max_single_trade_usd: f64,
    pub max_loss_usd: f64,
    pub window_end: DateTime<Utc>,
}

/// The Banker's decision on a credit proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditDecision {
    pub proposal_id: Uuid,
    pub approved: bool,
    pub approved_usd: Option<f64>,
    pub rejection_reason: Option<String>,
    pub score: f64,
    pub credit_line: Option<CreditLine>,
}

/// An agent's cumulative reputation across credit lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReputation {
    pub agent_id: Uuid,
    pub score: f64,
    pub lines_approved: u32,
    pub lines_repaid_cleanly: u32,
    pub lines_recalled: u32,
    pub avg_utilization_pct: f64,
    pub avg_return_pct: f64,
}

impl Default for AgentReputation {
    fn default() -> Self {
        Self {
            agent_id: Uuid::nil(),
            score: 5.0, // neutral starting score
            lines_approved: 0,
            lines_repaid_cleanly: 0,
            lines_recalled: 0,
            avg_utilization_pct: 0.0,
            avg_return_pct: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Trade proposals and guardian
// ---------------------------------------------------------------------------

/// A trade proposal submitted by an agent for guardian review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeProposal {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub submitted_at: DateTime<Utc>,
    pub pair: String,
    pub side: TradeSide,
    pub amount_usd: f64,
    pub confidence: f64,
    pub reasoning: String,
    /// For DeFi trades: the target contract address.
    pub contract_address: Option<String>,
    /// For DeFi trades: the method being called.
    pub contract_method: Option<String>,
}

/// Buy or sell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// Result of all guardian checks on a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardianResult {
    pub proposal_id: Uuid,
    pub approved: bool,
    pub risk_score: f64,
    pub checks: Vec<CheckResult>,
}

/// Result of a single guardian check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// x402 payment interception
// ---------------------------------------------------------------------------

/// An x402 payment request intercepted from an agent's HTTP 402 flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X402PaymentRequest {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub recipient: String,
    pub amount_usd: f64,
    pub currency: String,
    pub service_url: String,
    pub purpose: String,
    pub submitted_at: DateTime<Utc>,
}

/// Risk classification for an x402 payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum X402RiskLevel {
    /// Known recipient, small amount, matches strategy — auto-approve.
    Low,
    /// First-time recipient or unusual amount — dashboard alert for human review.
    Medium,
    /// Blocklisted address, exceeds budget, off-strategy — auto-block.
    High,
}

/// Guardian's verdict on an x402 payment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X402Verdict {
    pub payment_id: Uuid,
    pub approved: bool,
    pub risk_level: X402RiskLevel,
    pub reason: String,
    pub needs_human_review: bool,
}

/// A pending x402 payment awaiting human review on the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingX402Payment {
    pub payment: X402PaymentRequest,
    pub risk_level: X402RiskLevel,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Dashboard events
// ---------------------------------------------------------------------------

/// Events broadcast to the live WebSocket dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    AgentRegistered {
        agent: Agent,
    },
    ProposalSubmitted {
        proposal: TradeProposal,
    },
    GuardianVerdict {
        result: GuardianResult,
    },
    TradeExecuted {
        proposal_id: Uuid,
        agent_id: Uuid,
        pair: String,
        side: TradeSide,
        amount_usd: f64,
    },
    TradeRejected {
        proposal_id: Uuid,
        agent_id: Uuid,
        reason: String,
    },
    PortfolioUpdate {
        balances: HashMap<String, f64>,
        timestamp: DateTime<Utc>,
    },
    CreditProposalPending {
        proposal: CreditProposal,
        score: f64,
        recommended_usd: f64,
    },
    CreditApproved {
        credit_line: CreditLine,
    },
    CreditRejectedByHuman {
        proposal_id: Uuid,
        agent_id: Uuid,
    },
    CreditRecalled {
        agent_id: Uuid,
        reason: String,
    },
    CreditRepaid {
        agent_id: Uuid,
    },
    BudgetUpdate {
        agent_id: Uuid,
        spent_usd: f64,
        remaining_usd: f64,
    },
    X402PaymentPending {
        payment: X402PaymentRequest,
        risk: X402RiskLevel,
        reason: String,
    },
    X402PaymentApproved {
        payment_id: Uuid,
        agent_id: Uuid,
    },
    X402PaymentBlocked {
        payment_id: Uuid,
        agent_id: Uuid,
        reason: String,
    },
    Error {
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Dashboard snapshot (full state for /api/snapshot)
// ---------------------------------------------------------------------------

/// Full snapshot of system state served via the dashboard API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    pub agents: Vec<Agent>,
    pub pending_proposals: Vec<PendingProposalInfo>,
    pub pending_x402_payments: Vec<PendingX402Payment>,
    pub active_credit_lines: Vec<CreditLine>,
    pub recent_proposals: Vec<TradeProposal>,
    pub recent_guardian_results: Vec<GuardianResult>,
    pub portfolio: HashMap<String, f64>,
    pub reputations: Vec<AgentReputation>,
    pub timestamp: DateTime<Utc>,
}

/// Info about a pending credit proposal awaiting human approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingProposalInfo {
    pub proposal: CreditProposal,
    pub score: f64,
    pub recommended_usd: f64,
    pub submitted_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// MCP JSON-RPC types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    /// Create a success response.
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    pub fn error(id: serde_json::Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// MCP manifest types
// ---------------------------------------------------------------------------

/// MCP tool manifest for capability advertisement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub tools: Vec<McpTool>,
}

/// A single MCP tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Global policy configuration for the guardian.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Pairs allowed globally (if empty, all pairs are allowed).
    pub allowed_pairs: Vec<String>,
    /// Maximum USD per single trade across all agents.
    pub max_single_trade_usd: f64,
    /// Minimum agent confidence to proceed.
    pub min_confidence: f64,
    /// Maximum trades per agent per hour.
    pub max_trades_per_hour: u32,
    /// Whitelisted DeFi contract addresses.
    pub whitelisted_contracts: Vec<String>,
    /// Whitelisted DeFi contract methods.
    pub safe_methods: Vec<String>,
    /// Anomaly detection: max proposals per agent per minute.
    pub max_proposals_per_minute: u32,
    /// Anomaly detection: max cumulative risk score before flagging.
    pub max_cumulative_risk_score: f64,
    /// x402: known-good recipient addresses (auto-approve on Low risk).
    pub x402_allowed_recipients: Vec<String>,
    /// x402: blocklisted recipient addresses (auto-block on High risk).
    pub x402_blocked_recipients: Vec<String>,
    /// x402: max USD per single x402 payment.
    pub x402_max_payment_usd: f64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allowed_pairs: vec![
                "BTC-USDT".to_string(),
                "ETH-USDT".to_string(),
                "SOL-USDT".to_string(),
            ],
            max_single_trade_usd: 1.0,
            min_confidence: 0.40,
            max_trades_per_hour: 20,
            whitelisted_contracts: Vec::new(),
            safe_methods: vec![
                "swap".to_string(),
                "transfer".to_string(),
                "approve".to_string(),
            ],
            max_proposals_per_minute: 10,
            max_cumulative_risk_score: 50.0,
            x402_allowed_recipients: Vec::new(),
            x402_blocked_recipients: Vec::new(),
            x402_max_payment_usd: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Unified error type for the system.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum AppError {
    #[error("Agent not registered: {0}")]
    AgentNotRegistered(Uuid),

    #[error("No active credit line for agent: {0}")]
    NoCreditLine(Uuid),

    #[error("Credit line expired for agent: {0}")]
    CreditLineExpired(Uuid),

    #[error("Insufficient credit: requested {requested} but only {remaining} remaining")]
    InsufficientCredit { requested: f64, remaining: f64 },

    #[error("Guardian rejected proposal: {0}")]
    GuardianRejected(String),

    #[error("Trade execution failed: {0}")]
    ExecutionFailed(String),

    #[error("OKX API error: {0}")]
    OkxError(String),

    #[error("JSON serialization error: {0}")]
    SerdeError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("x402 payment blocked: {0}")]
    X402Blocked(String),

    #[error("x402 payment pending human review: {0}")]
    X402PendingReview(String),
}
