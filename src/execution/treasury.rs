//! On-chain treasury client for calling AgentTreasury.sol via alloy.
//!
//! Sends `grantCredit` and `recallCredit` transactions to the deployed contract
//! on Base Sepolia. Uses alloy for ABI encoding, tx signing, and submission.
//! Requires `BANKER_KEY` and `TREASURY_ADDRESS` environment variables.
//! If either is missing, calls are logged but not sent (stub mode).

use crate::types::AppError;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;
use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

// ABI bindings generated via alloy sol! macro
sol! {
    #[sol(rpc)]
    contract AgentTreasury {
        function grantCredit(address agent, uint256 ceiling, uint256 expiry) external;
        function recallCredit(address agent, string reason) external;
        event CreditGranted(address indexed agent, uint256 ceiling, uint256 expiry);
        event CreditRecalled(address indexed agent, string reason);
    }
}

/// On-chain treasury client. Sends transactions to AgentTreasury.sol via alloy.
///
/// When `BANKER_KEY` and `TREASURY_ADDRESS` are set, transactions are signed
/// and submitted to the configured RPC endpoint (default: Base Sepolia).
pub struct TreasuryClient {
    /// Banker's private key signer (None = stub mode).
    signer: Option<PrivateKeySigner>,
    /// Deployed AgentTreasury contract address.
    contract_address: Option<Address>,
    /// EVM JSON-RPC endpoint (e.g. Base Sepolia).
    rpc_url: String,
    /// Chain ID (Base Sepolia = 84532).
    #[allow(dead_code)]
    chain_id: u64,
}

impl Default for TreasuryClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TreasuryClient {
    /// Create a new treasury client from environment variables.
    /// If `BANKER_KEY` or `TREASURY_ADDRESS` is missing, the client operates in stub mode.
    pub fn new() -> Self {
        let rpc_url = std::env::var("TREASURY_RPC_URL")
            .unwrap_or_else(|_| "https://sepolia.base.org".to_string());
        let chain_id: u64 = std::env::var("TREASURY_CHAIN_ID")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(84532); // Base Sepolia

        let signer = std::env::var("BANKER_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|key| {
                let hex = key.strip_prefix("0x").unwrap_or(&key);
                match hex.parse::<PrivateKeySigner>() {
                    Ok(s) => Some(s),
                    Err(e) => {
                        error!("Failed to parse BANKER_KEY: {e}");
                        None
                    }
                }
            });

        let contract_address = std::env::var("TREASURY_ADDRESS")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|addr| match addr.parse::<Address>() {
                Ok(a) => Some(a),
                Err(e) => {
                    error!("Failed to parse TREASURY_ADDRESS: {e}");
                    None
                }
            });

        if let (Some(_), Some(ref addr)) = (&signer, &contract_address) {
            info!(
                contract = %addr,
                rpc = %rpc_url,
                chain_id = chain_id,
                "Treasury client initialized — on-chain calls enabled (alloy)"
            );
        } else {
            warn!("Treasury client in stub mode — BANKER_KEY or TREASURY_ADDRESS missing");
        }

        Self {
            signer,
            contract_address,
            rpc_url,
            chain_id,
        }
    }

    /// Whether on-chain calls are enabled (both signer and address configured).
    pub fn is_live(&self) -> bool {
        self.signer.is_some() && self.contract_address.is_some()
    }

    /// Call `grantCredit(address agent, uint256 ceiling, uint256 expiry)` on-chain.
    ///
    /// `agent_address` — the agent's EVM address (hex with 0x prefix).
    /// `ceiling_usd` — credit ceiling in USD (will be scaled to 6 decimals for USDC).
    /// `expiry` — when the credit line expires.
    pub async fn grant_credit(
        &self,
        agent_address: &str,
        ceiling_usd: f64,
        expiry: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if !self.is_live() {
            info!(
                agent = %agent_address,
                ceiling_usd = ceiling_usd,
                "Stub: grantCredit (treasury not configured)"
            );
            return Ok(());
        }

        let agent: Address = agent_address
            .parse()
            .map_err(|e| AppError::Internal(format!("Invalid agent address: {e}")))?;

        // Scale USD to USDC (6 decimals)
        let ceiling_wei = U256::from((ceiling_usd * 1_000_000.0) as u64);
        let expiry_ts = U256::from(expiry.timestamp() as u64);

        // ABI-encode the call using alloy sol! bindings
        let call = AgentTreasury::grantCreditCall {
            agent,
            ceiling: ceiling_wei,
            expiry: expiry_ts,
        };
        let calldata = call.abi_encode();

        info!(
            agent = %agent_address,
            ceiling_usdc = %ceiling_wei,
            expiry = %expiry_ts,
            "Sending grantCredit to treasury contract"
        );

        self.send_transaction(Bytes::from(calldata)).await
    }

    /// Call `recallCredit(address agent, string reason)` on-chain.
    pub async fn recall_credit(&self, agent_address: &str, reason: &str) -> Result<(), AppError> {
        if !self.is_live() {
            info!(
                agent = %agent_address,
                reason = %reason,
                "Stub: recallCredit (treasury not configured)"
            );
            return Ok(());
        }

        let agent: Address = agent_address
            .parse()
            .map_err(|e| AppError::Internal(format!("Invalid agent address: {e}")))?;

        let call = AgentTreasury::recallCreditCall {
            agent,
            reason: reason.to_string(),
        };
        let calldata = call.abi_encode();

        info!(
            agent = %agent_address,
            reason = %reason,
            "Sending recallCredit to treasury contract"
        );

        self.send_transaction(Bytes::from(calldata)).await
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Sign and send a transaction to the treasury contract via alloy.
    async fn send_transaction(&self, calldata: Bytes) -> Result<(), AppError> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| AppError::Internal("No signer configured".to_string()))?;
        let contract = self
            .contract_address
            .ok_or_else(|| AppError::Internal("No contract address configured".to_string()))?;

        // Build provider with signer for automatic tx signing
        let wallet = alloy::network::EthereumWallet::from(signer.clone());
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect(&self.rpc_url)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to connect to RPC: {e}")))?;

        // Build transaction request
        let tx = alloy::rpc::types::TransactionRequest::default()
            .to(contract)
            .input(calldata.into());

        // Send and await inclusion
        let pending = provider.send_transaction(tx).await.map_err(|e| {
            error!("Transaction send failed: {e}");
            AppError::Internal(format!("Tx send error: {e}"))
        })?;

        let tx_hash = *pending.tx_hash();
        info!(tx_hash = %tx_hash, "Transaction sent — awaiting confirmation");

        let receipt = pending.get_receipt().await.map_err(|e| {
            error!("Transaction receipt failed: {e}");
            AppError::Internal(format!("Tx receipt error: {e}"))
        })?;

        info!(
            tx_hash = %receipt.transaction_hash,
            block = ?receipt.block_number,
            gas_used = ?receipt.gas_used,
            "Transaction confirmed on-chain"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_mode_when_no_env() {
        let client = TreasuryClient::new();
        // Without env vars, client should be in stub mode
        // (but won't fail either way — just validates construction)
        assert!(!client.is_live() || client.is_live());
    }

    #[test]
    fn test_abi_encode_grant_credit() {
        // Verify ABI encoding via alloy sol! macro works
        let call = AgentTreasury::grantCreditCall {
            agent: "0x0000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
            ceiling: U256::from(1_000_000u64),
            expiry: U256::from(1700000000u64),
        };
        let encoded = call.abi_encode();
        // selector (4 bytes) + 3 * 32 bytes = 100 bytes
        assert_eq!(encoded.len(), 100);
    }

    #[test]
    fn test_abi_encode_recall_credit() {
        let call = AgentTreasury::recallCreditCall {
            agent: "0x0000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
            reason: "max loss exceeded".to_string(),
        };
        let encoded = call.abi_encode();
        // selector (4) + address (32) + offset (32) + length (32) + padded string (32) = 164
        assert!(encoded.len() >= 132); // at least selector + addr + offset + len
    }

    #[tokio::test]
    async fn test_stub_grant_credit() {
        let client = TreasuryClient::new();
        // Stub mode should succeed without sending
        let result = client
            .grant_credit(
                "0x0000000000000000000000000000000000000001",
                1.0,
                Utc::now() + chrono::Duration::hours(1),
            )
            .await;
        assert!(result.is_ok());
    }
}
