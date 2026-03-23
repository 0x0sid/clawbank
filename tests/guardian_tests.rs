//! Integration tests for the Guardian module.

use chrono::{Duration, Utc};
use openclaw_aibank::guardian::Guardian;
use openclaw_aibank::types::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

fn make_tx() -> broadcast::Sender<DashboardEvent> {
    let (tx, _) = broadcast::channel(100);
    tx
}

fn make_credit_line(agent_id: Uuid) -> CreditLine {
    CreditLine {
        id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        agent_id,
        approved_usd: 5.0,
        spent_usd: 0.0,
        remaining_usd: 5.0,
        status: CreditStatus::Active,
        approved_at: Utc::now(),
        expires_at: Utc::now() + Duration::hours(24),
        conditions: ApprovedConditions {
            allowed_pairs: vec!["BTC-USDT".to_string(), "ETH-USDT".to_string()],
            max_single_trade_usd: 1.0,
            max_loss_usd: 3.0,
            window_end: Utc::now() + Duration::hours(24),
        },
        reputation_at_approval: 5.0,
    }
}

fn make_proposal(agent_id: Uuid) -> TradeProposal {
    TradeProposal {
        id: Uuid::new_v4(),
        agent_id,
        submitted_at: Utc::now(),
        pair: "BTC-USDT".to_string(),
        side: TradeSide::Buy,
        amount_usd: 0.50,
        confidence: 0.75,
        reasoning: "RSI oversold".to_string(),
        contract_address: None,
        contract_method: None,
    }
}

#[tokio::test]
async fn guardian_approves_valid_proposal() {
    let agent_id = Uuid::new_v4();
    let credit_lines = Arc::new(RwLock::new(HashMap::new()));
    credit_lines
        .write()
        .await
        .insert(agent_id, make_credit_line(agent_id));

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let result = guardian.verify(&make_proposal(agent_id)).await.unwrap();

    assert!(result.approved);
    assert_eq!(result.checks.len(), 6);
    assert!(result.checks.iter().all(|c| c.passed));
}

#[tokio::test]
async fn guardian_rejects_without_credit_line() {
    let agent_id = Uuid::new_v4();
    let credit_lines = Arc::new(RwLock::new(HashMap::new()));

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let result = guardian.verify(&make_proposal(agent_id)).await.unwrap();

    assert!(!result.approved);
    assert_eq!(result.checks[0].name, "check_credit_line");
    assert!(!result.checks[0].passed);
}

#[tokio::test]
async fn guardian_rejects_expired_credit_line() {
    let agent_id = Uuid::new_v4();
    let mut line = make_credit_line(agent_id);
    line.expires_at = Utc::now() - Duration::hours(1); // expired

    let credit_lines = Arc::new(RwLock::new(HashMap::new()));
    credit_lines.write().await.insert(agent_id, line);

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let result = guardian.verify(&make_proposal(agent_id)).await.unwrap();

    assert!(!result.approved);
    assert!(!result.checks[0].passed);
}

#[tokio::test]
async fn guardian_rejects_disallowed_pair() {
    let agent_id = Uuid::new_v4();
    let credit_lines = Arc::new(RwLock::new(HashMap::new()));
    credit_lines
        .write()
        .await
        .insert(agent_id, make_credit_line(agent_id));

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let mut proposal = make_proposal(agent_id);
    proposal.pair = "DOGE-USDT".to_string();

    let result = guardian.verify(&proposal).await.unwrap();
    assert!(!result.approved);
}

#[tokio::test]
async fn guardian_rejects_low_confidence() {
    let agent_id = Uuid::new_v4();
    let credit_lines = Arc::new(RwLock::new(HashMap::new()));
    credit_lines
        .write()
        .await
        .insert(agent_id, make_credit_line(agent_id));

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let mut proposal = make_proposal(agent_id);
    proposal.confidence = 0.10; // well below 40%

    let result = guardian.verify(&proposal).await.unwrap();
    assert!(!result.approved);
    assert!(!result.checks[2].passed); // check_confidence is index 2
}

#[tokio::test]
async fn guardian_rejects_over_budget() {
    let agent_id = Uuid::new_v4();
    let mut line = make_credit_line(agent_id);
    line.remaining_usd = 0.20;

    let credit_lines = Arc::new(RwLock::new(HashMap::new()));
    credit_lines.write().await.insert(agent_id, line);

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let proposal = make_proposal(agent_id); // requests $0.50, exceeds $0.20 remaining

    let result = guardian.verify(&proposal).await.unwrap();
    assert!(!result.approved);
    assert!(!result.checks[0].passed);
}

#[tokio::test]
async fn guardian_credit_line_check_is_always_first() {
    let agent_id = Uuid::new_v4();
    let credit_lines = Arc::new(RwLock::new(HashMap::new()));

    let guardian = Guardian::new(credit_lines, PolicyConfig::default(), make_tx());
    let result = guardian.verify(&make_proposal(agent_id)).await.unwrap();

    // Regardless of outcome, check_credit_line must be the first check
    assert_eq!(result.checks[0].name, "check_credit_line");
}
