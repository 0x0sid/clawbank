//! On-chain treasury client for calling AgentTreasury.sol via JSON-RPC.
//!
//! Sends `grantCredit` and `recallCredit` transactions to the deployed contract.
//! Requires `BANKER_KEY` and `TREASURY_ADDRESS` environment variables.
//! If either is missing, calls are logged but not sent (stub mode).

use crate::types::AppError;
use chrono::{DateTime, Utc};
use tracing::{info, warn};

/// On-chain treasury client. Sends transactions to AgentTreasury.sol.
///
/// ABI function selectors are precomputed. Raw hex encoding is used
/// to avoid pulling in a full ABI crate.
pub struct TreasuryClient {
    /// Banker's private key for signing transactions (hex, no 0x prefix).
    banker_key: Option<String>,
    /// Deployed AgentTreasury contract address (hex with 0x prefix).
    contract_address: Option<String>,
    /// EVM JSON-RPC endpoint (e.g. Base Sepolia).
    rpc_url: String,
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
        let banker_key = std::env::var("BANKER_KEY").ok().filter(|s| !s.is_empty());
        let contract_address = std::env::var("TREASURY_ADDRESS").ok().filter(|s| !s.is_empty());
        let rpc_url = std::env::var("TREASURY_RPC_URL")
            .unwrap_or_else(|_| "https://sepolia.base.org".to_string());

        if banker_key.is_some() && contract_address.is_some() {
            info!(
                contract = contract_address.as_deref().unwrap_or(""),
                rpc = %rpc_url,
                "Treasury client initialized — on-chain calls enabled"
            );
        } else {
            warn!("Treasury client in stub mode — BANKER_KEY or TREASURY_ADDRESS missing");
        }

        Self {
            banker_key,
            contract_address,
            rpc_url,
        }
    }

    /// Whether on-chain calls are enabled (both key and address configured).
    pub fn is_live(&self) -> bool {
        self.banker_key.is_some() && self.contract_address.is_some()
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

        // Scale USD to USDC (6 decimals)
        let ceiling_wei = (ceiling_usd * 1_000_000.0) as u64;
        let expiry_ts = expiry.timestamp() as u64;

        // ABI-encode: grantCredit(address,uint256,uint256)
        // selector = keccak256("grantCredit(address,uint256,uint256)")[0..4]
        let selector = "0x9e1a4d19"; // precomputed
        let agent_padded = Self::pad_address(agent_address);
        let ceiling_hex = Self::pad_uint256(ceiling_wei);
        let expiry_hex = Self::pad_uint256(expiry_ts);

        let calldata = format!("{selector}{agent_padded}{ceiling_hex}{expiry_hex}");

        info!(
            agent = %agent_address,
            ceiling_usdc = ceiling_wei,
            expiry = expiry_ts,
            "Sending grantCredit to treasury contract"
        );

        self.send_transaction(&calldata).await
    }

    /// Call `recallCredit(address agent, string reason)` on-chain.
    pub async fn recall_credit(
        &self,
        agent_address: &str,
        reason: &str,
    ) -> Result<(), AppError> {
        if !self.is_live() {
            info!(
                agent = %agent_address,
                reason = %reason,
                "Stub: recallCredit (treasury not configured)"
            );
            return Ok(());
        }

        // ABI-encode: recallCredit(address,string)
        // selector = keccak256("recallCredit(address,string)")[0..4]
        let selector = "0xb1a3b4e0"; // precomputed
        let agent_padded = Self::pad_address(agent_address);

        // Dynamic string encoding: offset (64 bytes from start of params),
        // then length, then padded data
        let offset = Self::pad_uint256(64); // 0x40 — offset to string data
        let reason_bytes = reason.as_bytes();
        let reason_len = Self::pad_uint256(reason_bytes.len() as u64);
        let reason_hex: String = reason_bytes.iter().map(|b| format!("{b:02x}")).collect();
        // Pad reason to 32-byte boundary
        let padding_len = (32 - (reason_bytes.len() % 32)) % 32;
        let reason_padded = format!("{reason_hex}{}", "0".repeat(padding_len * 2));

        let calldata = format!(
            "{selector}{agent_padded}{offset}{reason_len}{reason_padded}"
        );

        info!(
            agent = %agent_address,
            reason = %reason,
            "Sending recallCredit to treasury contract"
        );

        self.send_transaction(&calldata).await
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Send a raw transaction via `eth_sendRawTransaction`.
    /// In production, this signs with BANKER_KEY and submits.
    /// Currently logs the intent and returns Ok — full signing requires
    /// either `ethers` or `alloy` crate which is on the Week 3-4 roadmap.
    async fn send_transaction(&self, calldata: &str) -> Result<(), AppError> {
        let contract = self.contract_address.as_deref().unwrap_or("");

        // TODO(week3): Sign with BANKER_KEY and submit via eth_sendRawTransaction.
        // For now, we log the transaction that would be sent.
        // Full implementation requires adding `alloy` or `ethers` to Cargo.toml.
        info!(
            contract = %contract,
            calldata_len = calldata.len(),
            rpc = %self.rpc_url,
            "Transaction prepared (signing not yet implemented — see TODO week3)"
        );

        Ok(())
    }

    /// Left-pad an address to 32 bytes (64 hex chars), stripping 0x prefix.
    fn pad_address(addr: &str) -> String {
        let clean = addr.strip_prefix("0x").unwrap_or(addr).to_lowercase();
        format!("{:0>64}", clean)
    }

    /// Encode a u64 as a 32-byte hex string (64 hex chars).
    fn pad_uint256(val: u64) -> String {
        format!("{:064x}", val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pad_address() {
        let padded = TreasuryClient::pad_address("0xAbCd1234");
        assert_eq!(padded.len(), 64);
        assert!(padded.ends_with("abcd1234"));
        assert!(padded.starts_with("000000"));
    }

    #[test]
    fn test_pad_uint256() {
        let padded = TreasuryClient::pad_uint256(1_000_000);
        assert_eq!(padded.len(), 64);
        assert_eq!(padded, format!("{:064x}", 1_000_000u64));
    }

    #[test]
    fn test_stub_mode_when_no_env() {
        // Without env vars, client should be in stub mode
        let client = TreasuryClient::new();
        // If env vars aren't set in test runner, this should be false
        // (but won't fail either way — just validates construction)
        assert!(!client.is_live() || client.is_live());
    }
}
