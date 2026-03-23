//! Integration tests for x402 payment interception and banker x402 workflow.

use chrono::{Duration, Utc};
use openclaw_aibank::banker::Banker;
use openclaw_aibank::execution::x402::{classify_risk, intercept_x402};
use openclaw_aibank::types::*;
use tokio::sync::broadcast;
use uuid::Uuid;

fn make_tx() -> broadcast::Sender<DashboardEvent> {
    let (tx, _) = broadcast::channel(100);
    tx
}

fn make_policy() -> PolicyConfig {
    PolicyConfig {
        x402_allowed_recipients: vec!["0xTrustedAPI".to_string()],
        x402_blocked_recipients: vec!["0xScamDrain".to_string()],
        x402_max_payment_usd: 1.0,
        ..PolicyConfig::default()
    }
}

fn make_payment(agent_id: Uuid, recipient: &str, amount: f64) -> X402PaymentRequest {
    X402PaymentRequest {
        id: Uuid::new_v4(),
        agent_id,
        recipient: recipient.to_string(),
        amount_usd: amount,
        currency: "USDC".to_string(),
        service_url: "https://api.example.com/data".to_string(),
        purpose: "Market data feed".to_string(),
        submitted_at: Utc::now(),
    }
}

fn good_proposal(agent_id: Uuid) -> CreditProposal {
    CreditProposal {
        id: Uuid::new_v4(),
        agent_id,
        submitted_at: Utc::now(),
        requested_usd: 5_000.0,
        max_loss_usd: 500.0,
        target_return_pct: 15.0,
        window_start: Utc::now(),
        window_end: Utc::now() + Duration::hours(48),
        strategy: "Mean reversion strategy on BTC-USDT using RSI and Bollinger Bands on the 4h timeframe. Enter on RSI < 30 with price at lower band, exit at middle band or RSI > 70. Tight stop at 10% of capital.".to_string(),
        allowed_pairs: vec!["BTC-USDT".to_string(), "ETH-USDT".to_string()],
        max_single_trade_usd: 2_500.0,
        repayment_trigger: RepaymentTrigger::ProfitTarget { pct: 15.0 },
        collateral: Some(Collateral {
            asset: "USDT".to_string(),
            amount: 2_500.0,
            locked_at: Utc::now(),
        }),
    }
}

// ---------------------------------------------------------------------------
// classify_risk integration tests
// ---------------------------------------------------------------------------

#[test]
fn x402_blocklisted_is_case_insensitive() {
    let policy = make_policy();
    let line = CreditLine {
        id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        approved_usd: 5.0,
        spent_usd: 0.0,
        remaining_usd: 5.0,
        status: CreditStatus::Active,
        approved_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(24),
        conditions: ApprovedConditions {
            allowed_pairs: vec!["BTC-USDT".to_string()],
            max_single_trade_usd: 1.0,
            max_loss_usd: 3.0,
            window_end: Utc::now() + Duration::hours(24),
        },
        reputation_at_approval: 7.0,
    };

    // Mixed case should still match blocklist
    let payment = make_payment(line.agent_id, "0xSCAMDRAIN", 0.50);
    let (risk, _) = classify_risk(&payment, Some(&line), &policy);
    assert_eq!(risk, X402RiskLevel::High);
}

#[test]
fn x402_allowlist_is_case_insensitive() {
    let policy = make_policy();
    let line = CreditLine {
        id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        approved_usd: 5.0,
        spent_usd: 0.0,
        remaining_usd: 5.0,
        status: CreditStatus::Active,
        approved_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(24),
        conditions: ApprovedConditions {
            allowed_pairs: vec!["BTC-USDT".to_string()],
            max_single_trade_usd: 1.0,
            max_loss_usd: 3.0,
            window_end: Utc::now() + Duration::hours(24),
        },
        reputation_at_approval: 7.0,
    };

    // Mixed case should still match allowlist → Low risk
    let payment = make_payment(line.agent_id, "0xTRUSTEDAPI", 0.25);
    let (risk, _) = classify_risk(&payment, Some(&line), &policy);
    assert_eq!(risk, X402RiskLevel::Low);
}

#[test]
fn x402_exceeds_remaining_budget_high_risk() {
    let policy = make_policy();
    let line = CreditLine {
        id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        approved_usd: 5.0,
        spent_usd: 4.80,
        remaining_usd: 0.20,
        status: CreditStatus::Active,
        approved_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(24),
        conditions: ApprovedConditions {
            allowed_pairs: vec!["BTC-USDT".to_string()],
            max_single_trade_usd: 1.0,
            max_loss_usd: 3.0,
            window_end: Utc::now() + Duration::hours(24),
        },
        reputation_at_approval: 7.0,
    };

    let payment = make_payment(line.agent_id, "0xTrustedAPI", 0.50);
    let (risk, reason) = classify_risk(&payment, Some(&line), &policy);
    assert_eq!(risk, X402RiskLevel::High);
    assert!(reason.contains("exceeds remaining budget"));
}

// ---------------------------------------------------------------------------
// intercept_x402 integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn x402_intercept_low_risk_auto_approves() {
    let policy = make_policy();
    let line = CreditLine {
        id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        approved_usd: 5.0,
        spent_usd: 0.0,
        remaining_usd: 5.0,
        status: CreditStatus::Active,
        approved_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(24),
        conditions: ApprovedConditions {
            allowed_pairs: vec!["BTC-USDT".to_string()],
            max_single_trade_usd: 1.0,
            max_loss_usd: 3.0,
            window_end: Utc::now() + Duration::hours(24),
        },
        reputation_at_approval: 7.0,
    };

    let payment = make_payment(line.agent_id, "0xTrustedAPI", 0.25);
    let verdict = intercept_x402(&payment, Some(&line), &policy)
        .await
        .expect("verdict");

    assert!(verdict.approved);
    assert_eq!(verdict.risk_level, X402RiskLevel::Low);
    assert!(!verdict.needs_human_review);
}

#[tokio::test]
async fn x402_intercept_medium_needs_review() {
    let policy = make_policy();
    let line = CreditLine {
        id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        approved_usd: 5.0,
        spent_usd: 0.0,
        remaining_usd: 5.0,
        status: CreditStatus::Active,
        approved_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(24),
        conditions: ApprovedConditions {
            allowed_pairs: vec!["BTC-USDT".to_string()],
            max_single_trade_usd: 1.0,
            max_loss_usd: 3.0,
            window_end: Utc::now() + Duration::hours(24),
        },
        reputation_at_approval: 7.0,
    };

    let payment = make_payment(line.agent_id, "0xUnknownAddr", 0.50);
    let verdict = intercept_x402(&payment, Some(&line), &policy)
        .await
        .expect("verdict");

    assert!(!verdict.approved);
    assert_eq!(verdict.risk_level, X402RiskLevel::Medium);
    assert!(verdict.needs_human_review);
}

// ---------------------------------------------------------------------------
// Banker x402 workflow: store → approve/block
// ---------------------------------------------------------------------------

#[tokio::test]
async fn banker_x402_store_and_approve() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("x402-agent".to_string(), None).await;
    let proposal = good_proposal(agent.id);
    banker.evaluate(&proposal).await;
    banker
        .approve_proposal(proposal.id, None)
        .await
        .expect("approve credit");

    let payment = make_payment(agent.id, "0xSomeAPI", 0.25);
    let payment_id = payment.id;

    // Store as pending
    banker
        .store_pending_x402(
            payment.clone(),
            X402RiskLevel::Medium,
            "Unknown recipient".to_string(),
        )
        .await;

    // Verify it appears in pending list
    let pending = banker.get_pending_x402().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].payment.id, payment_id);

    // Approve it — should deduct from credit line
    banker.approve_x402(payment_id).await.expect("approve x402");

    // Pending list should be empty
    assert!(banker.get_pending_x402().await.is_empty());

    // Credit line should have the deduction
    let line = banker.get_active_line(agent.id).await.expect("line");
    assert!((line.spent_usd - 0.25).abs() < 0.01);
}

#[tokio::test]
async fn banker_x402_store_and_block() {
    let banker = Banker::new(make_tx());
    let agent = banker
        .register_agent("x402-block-agent".to_string(), None)
        .await;
    let proposal = good_proposal(agent.id);
    banker.evaluate(&proposal).await;
    banker
        .approve_proposal(proposal.id, None)
        .await
        .expect("approve credit");

    let payment = make_payment(agent.id, "0xSuspicious", 0.50);
    let payment_id = payment.id;

    banker
        .store_pending_x402(
            payment,
            X402RiskLevel::Medium,
            "Suspicious recipient".to_string(),
        )
        .await;

    // Block it
    banker
        .block_x402(payment_id, "Rejected by operator".to_string())
        .await
        .expect("block x402");

    // Pending list should be empty
    assert!(banker.get_pending_x402().await.is_empty());

    // Credit line should NOT have any deduction
    let line = banker.get_active_line(agent.id).await.expect("line");
    assert!((line.spent_usd - 0.0).abs() < 0.01);
}

#[tokio::test]
async fn banker_x402_approve_nonexistent_fails() {
    let banker = Banker::new(make_tx());
    let result = banker.approve_x402(Uuid::new_v4()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn banker_x402_block_nonexistent_fails() {
    let banker = Banker::new(make_tx());
    let result = banker.block_x402(Uuid::new_v4(), "test".to_string()).await;
    assert!(result.is_err());
}
