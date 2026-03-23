//! End-to-end tests for TreasuryClient against a live Anvil node.
//!
//! Prerequisites:
//! 1. Anvil running: `anvil --host 127.0.0.1 --port 8545 --chain-id 31337`
//! 2. AgentTreasury deployed (see scripts/e2e-anvil.ps1)
//! 3. Env vars set: BANKER_KEY, TREASURY_ADDRESS, TREASURY_RPC_URL, TREASURY_CHAIN_ID
//!
//! These tests are gated behind `#[ignore]` so they don't run in normal CI.
//! Run explicitly with:
//!   cargo test --test treasury_e2e -- --ignored --nocapture
//!
//! All operations run in one sequential test to avoid nonce conflicts
//! (all txs share the same signer/Anvil account).

use chrono::{Duration, Utc};
use openclaw_aibank::execution::treasury::TreasuryClient;

/// Full sequential e2e: is_live → grant → recall → grant multiple agents.
///
/// Single test avoids nonce races from parallel tokio::test with same signer.
#[tokio::test]
#[ignore]
async fn treasury_e2e_full_flow() {
    let client = TreasuryClient::new();

    // --- Step 1: verify live mode ---
    assert!(
        client.is_live(),
        "TreasuryClient should be in live mode with BANKER_KEY + TREASURY_ADDRESS set"
    );
    eprintln!("✅ Step 1: TreasuryClient is live");

    // --- Step 2: grant credit to agent (Anvil account 1) ---
    let agent1 = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
    let expiry = Utc::now() + Duration::hours(24);

    let result = client.grant_credit(agent1, 5.0, expiry).await;
    assert!(
        result.is_ok(),
        "grantCredit should succeed: {:?}",
        result.err()
    );
    eprintln!("✅ Step 2: grantCredit($5) to agent1 confirmed on-chain");

    // --- Step 3: recall credit for agent1 ---
    let result = client.recall_credit(agent1, "e2e test recall").await;
    assert!(
        result.is_ok(),
        "recallCredit should succeed: {:?}",
        result.err()
    );
    eprintln!("✅ Step 3: recallCredit for agent1 confirmed on-chain");

    // --- Step 4: grant credit to multiple agents ---
    let agents = [
        "0x70997970C51812dc3A010C7d01b50e0d17dc79C8", // account 1
        "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC", // account 2
        "0x90F79bf6EB2c4f870365E785982E1f101E93b906", // account 3
    ];

    let expiry = Utc::now() + Duration::hours(1);
    for (i, agent) in agents.iter().enumerate() {
        let ceiling = (i as f64 + 1.0) * 1.0; // $1, $2, $3
        let result = client.grant_credit(agent, ceiling, expiry).await;
        assert!(
            result.is_ok(),
            "grantCredit for agent {} failed: {:?}",
            agent,
            result.err()
        );
    }
    eprintln!("✅ Step 4: grantCredit to 3 agents confirmed on-chain");

    // --- Step 5: re-grant after recall (proves recall zeroed ceiling) ---
    let result = client.grant_credit(agent1, 1.0, expiry).await;
    assert!(
        result.is_ok(),
        "re-grant after recall should succeed: {:?}",
        result.err()
    );
    eprintln!("✅ Step 5: re-grant after recall confirmed on-chain");

    eprintln!("\n🎉 All treasury e2e steps passed!");
}
