//! Integration tests for the Banker module.

use openclaw_aibank::types::*;
use openclaw_aibank::banker::Banker;
use chrono::{Duration, Utc};
use tokio::sync::broadcast;
use uuid::Uuid;

fn make_tx() -> broadcast::Sender<DashboardEvent> {
    let (tx, _) = broadcast::channel(100);
    tx
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

#[tokio::test]
async fn banker_register_and_check() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("integration-test".to_string()).await;
    assert!(banker.is_registered(agent.id).await);
    assert!(!banker.is_registered(Uuid::new_v4()).await);
}

#[tokio::test]
async fn banker_approve_good_proposal() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("good".to_string()).await;
    let proposal = good_proposal(agent.id);
    let decision = banker.evaluate(&proposal).await;

    assert!(decision.approved);
    assert!(decision.score >= 6.0);
    assert!(decision.approved_usd.is_some());
    assert!(decision.credit_line.is_some());
}

#[tokio::test]
async fn banker_reject_bad_proposal() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("risky".to_string()).await;

    let proposal = CreditProposal {
        id: Uuid::new_v4(),
        agent_id: agent.id,
        submitted_at: Utc::now(),
        requested_usd: 1_000_000.0,
        max_loss_usd: 900_000.0,
        target_return_pct: 1000.0,
        window_start: Utc::now(),
        window_end: Utc::now() + Duration::minutes(5),
        strategy: "yolo".to_string(),
        allowed_pairs: vec![],
        max_single_trade_usd: 0.0,
        repayment_trigger: RepaymentTrigger::Manual,
        collateral: None,
    };

    let decision = banker.evaluate(&proposal).await;
    assert!(!decision.approved);
    assert!(decision.score < 6.0);
    assert!(decision.rejection_reason.is_some());
}

#[tokio::test]
async fn banker_deduct_insufficient_credit() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("deduct-test".to_string()).await;
    let proposal = good_proposal(agent.id);
    let decision = banker.evaluate(&proposal).await;
    assert!(decision.approved);

    let approved = decision.approved_usd.unwrap();
    let result = banker.deduct(agent.id, approved + 1.0).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn banker_recall_updates_reputation() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("recall-rep-test".to_string()).await;

    let proposal = good_proposal(agent.id);
    banker.evaluate(&proposal).await;

    let rep_before = banker.reputation(agent.id).await;
    banker
        .recall(agent.id, "test recall".to_string())
        .await
        .unwrap();
    let rep_after = banker.reputation(agent.id).await;

    assert_eq!(rep_after.lines_recalled, rep_before.lines_recalled + 1);
    assert!(rep_after.score < rep_before.score);
}

#[tokio::test]
async fn banker_repay_updates_reputation_positively() {
    let banker = Banker::new(make_tx());
    let agent = banker.register_agent("repay-test".to_string()).await;

    let proposal = good_proposal(agent.id);
    let decision = banker.evaluate(&proposal).await;
    assert!(decision.approved);

    let rep_before = banker.reputation(agent.id).await;
    banker.repay(agent.id).await.unwrap();
    let rep_after = banker.reputation(agent.id).await;

    assert_eq!(
        rep_after.lines_repaid_cleanly,
        rep_before.lines_repaid_cleanly + 1
    );
    assert!(rep_after.score >= rep_before.score);
}

#[tokio::test]
async fn banker_no_active_line_for_unregistered() {
    let banker = Banker::new(make_tx());
    assert!(banker.get_active_line(Uuid::new_v4()).await.is_none());
}
