//! OKX REST API client with HMAC-SHA256 signing.
//!
//! Used by the portfolio poller to fetch real balances and positions.
//! Credentials come from environment variables — never hardcoded.

use crate::types::AppError;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;
use std::collections::HashMap;
use tracing::{error, info, warn};

type HmacSha256 = Hmac<Sha256>;

const OKX_BASE_URL: &str = "https://www.okx.com";

/// OKX REST API credentials loaded from environment.
#[derive(Clone)]
pub struct OkxCredentials {
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
}

impl OkxCredentials {
    /// Load credentials from environment variables.
    /// Returns None if any required var is missing.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("OKX_API_KEY").ok()?;
        let secret_key = std::env::var("OKX_SECRET_KEY").ok()?;
        let passphrase = std::env::var("OKX_PASSPHRASE").ok()?;

        if api_key.is_empty() || secret_key.is_empty() || passphrase.is_empty() {
            return None;
        }

        Some(Self {
            api_key,
            secret_key,
            passphrase,
        })
    }
}

/// OKX REST client for portfolio queries and order management.
pub struct OkxRestClient {
    client: Client,
    credentials: Option<OkxCredentials>,
}

impl Default for OkxRestClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OkxRestClient {
    /// Create a new OKX REST client. If credentials are not available,
    /// all calls return stub/simulated data.
    pub fn new() -> Self {
        let credentials = OkxCredentials::from_env();
        if credentials.is_some() {
            info!("OKX REST client initialized with credentials");
        } else {
            warn!("OKX REST client initialized WITHOUT credentials — using simulated data");
        }

        Self {
            client: Client::new(),
            credentials,
        }
    }

    /// Fetch account balances from OKX. Returns a map of asset -> balance.
    pub async fn get_balances(&self) -> Result<HashMap<String, f64>, AppError> {
        let creds = match &self.credentials {
            Some(c) => c,
            None => return Ok(self.simulated_balances()),
        };

        let path = "/api/v5/account/balance";
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let sign = self.sign(creds, &timestamp, "GET", path, "")?;

        let resp = self
            .client
            .get(format!("{OKX_BASE_URL}{path}"))
            .header("OK-ACCESS-KEY", &creds.api_key)
            .header("OK-ACCESS-SIGN", &sign)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", &creds.passphrase)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| AppError::OkxError(format!("Balance request failed: {e}")))?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::OkxError(format!("Balance response parse failed: {e}")))?;

        if !status.is_success() {
            error!(status = %status, body = %body, "OKX balance API error");
            return Err(AppError::OkxError(format!(
                "OKX API returned {status}: {body}"
            )));
        }

        // Parse OKX balance response
        let mut balances = HashMap::new();
        if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            for account in data {
                if let Some(details) = account.get("details").and_then(|d| d.as_array()) {
                    for detail in details {
                        let ccy = detail
                            .get("ccy")
                            .and_then(|v| v.as_str())
                            .unwrap_or("UNKNOWN");
                        let eq: f64 = detail
                            .get("eq")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0.0);
                        if eq > 0.0 {
                            balances.insert(ccy.to_string(), eq);
                        }
                    }
                }
            }
        }

        info!(assets = balances.len(), "Portfolio balances fetched from OKX");
        Ok(balances)
    }

    /// Cancel all pending orders for a given instrument.
    pub async fn cancel_all_orders(&self, inst_id: &str) -> Result<(), AppError> {
        let creds = match &self.credentials {
            Some(c) => c,
            None => {
                warn!(inst_id = %inst_id, "Cannot cancel orders — no OKX credentials");
                return Ok(());
            }
        };

        let path = "/api/v5/trade/cancel-batch-orders";
        let body_json = serde_json::json!([{
            "instId": inst_id
        }]);
        let body_str = serde_json::to_string(&body_json)
            .map_err(|e| AppError::OkxError(format!("Serialize error: {e}")))?;

        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let sign = self.sign(creds, &timestamp, "POST", path, &body_str)?;

        let resp = self
            .client
            .post(format!("{OKX_BASE_URL}{path}"))
            .header("OK-ACCESS-KEY", &creds.api_key)
            .header("OK-ACCESS-SIGN", &sign)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", &creds.passphrase)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| AppError::OkxError(format!("Cancel orders request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or(serde_json::Value::Null);
            error!(
                inst_id = %inst_id,
                status = %status,
                body = %body,
                "OKX cancel orders failed"
            );
            return Err(AppError::OkxError(format!(
                "Cancel orders returned {status}"
            )));
        }

        info!(inst_id = %inst_id, "Cancel all orders request sent to OKX");
        Ok(())
    }

    /// Place a spot market order on OKX.
    ///
    /// `inst_id` — instrument like "BTC-USDT"
    /// `side` — "buy" or "sell"
    /// `amount_usd` — dollar amount to spend (for buys, uses tgtCcy=quote_ccy)
    pub async fn place_market_order(
        &self,
        inst_id: &str,
        side: &str,
        amount_usd: f64,
    ) -> Result<serde_json::Value, AppError> {
        let creds = match &self.credentials {
            Some(c) => c,
            None => {
                return Err(AppError::OkxError(
                    "Cannot place order — no OKX credentials".to_string(),
                ))
            }
        };

        let path = "/api/v5/trade/order";
        let body_json = serde_json::json!({
            "instId": inst_id,
            "tdMode": "cash",
            "side": side,
            "ordType": "market",
            "sz": format!("{:.2}", amount_usd),
            "tgtCcy": "quote_ccy"
        });
        let body_str = serde_json::to_string(&body_json)
            .map_err(|e| AppError::OkxError(format!("Serialize error: {e}")))?;

        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let sign = self.sign(creds, &timestamp, "POST", path, &body_str)?;

        let resp = self
            .client
            .post(format!("{OKX_BASE_URL}{path}"))
            .header("OK-ACCESS-KEY", &creds.api_key)
            .header("OK-ACCESS-SIGN", &sign)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", &creds.passphrase)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| AppError::OkxError(format!("Place order request failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::OkxError(format!("Place order response parse failed: {e}")))?;

        let code = body
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("?");

        if code != "0" {
            let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
            let detail = body
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
                .and_then(|d| d.get("sMsg"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            error!(
                inst_id = %inst_id,
                code = %code,
                msg = %msg,
                detail = %detail,
                "OKX place order failed"
            );
            return Err(AppError::OkxError(format!(
                "OKX order failed: code={code} msg={msg} detail={detail}"
            )));
        }

        let order_id = body
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|d| d.get("ordId"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        info!(
            inst_id = %inst_id,
            side = %side,
            amount_usd = amount_usd,
            order_id = %order_id,
            "OKX market order placed successfully"
        );

        Ok(body)
    }

    /// Get open positions for P&L tracking.
    pub async fn get_positions(&self) -> Result<Vec<OkxPosition>, AppError> {
        let creds = match &self.credentials {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };

        let path = "/api/v5/account/positions";
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let sign = self.sign(creds, &timestamp, "GET", path, "")?;

        let resp = self
            .client
            .get(format!("{OKX_BASE_URL}{path}"))
            .header("OK-ACCESS-KEY", &creds.api_key)
            .header("OK-ACCESS-SIGN", &sign)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", &creds.passphrase)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| AppError::OkxError(format!("Positions request failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::OkxError(format!("Positions parse failed: {e}")))?;

        let mut positions = Vec::new();
        if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            for pos in data {
                let inst_id = pos
                    .get("instId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let upl: f64 = pos
                    .get("upl")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                let notional_usd: f64 = pos
                    .get("notionalUsd")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);

                if !inst_id.is_empty() {
                    positions.push(OkxPosition {
                        inst_id,
                        unrealized_pnl: upl,
                        notional_usd,
                    });
                }
            }
        }

        info!(count = positions.len(), "Positions fetched from OKX");
        Ok(positions)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// HMAC-SHA256 signature for OKX API requests.
    fn sign(
        &self,
        creds: &OkxCredentials,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, AppError> {
        let prehash = format!("{timestamp}{method}{path}{body}");
        let mut mac = HmacSha256::new_from_slice(creds.secret_key.as_bytes())
            .map_err(|e| AppError::OkxError(format!("HMAC key error: {e}")))?;
        mac.update(prehash.as_bytes());
        let result = mac.finalize().into_bytes();
        Ok(base64::engine::general_purpose::STANDARD.encode(result))
    }

    /// Simulated balances when no OKX credentials are available.
    fn simulated_balances(&self) -> HashMap<String, f64> {
        let mut balances = HashMap::new();
        balances.insert("USDT".to_string(), 10_000.0);
        balances.insert("BTC".to_string(), 0.0);
        balances.insert("ETH".to_string(), 0.0);
        balances
    }
}

/// A parsed OKX position for P&L tracking.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OkxPosition {
    pub inst_id: String,
    pub unrealized_pnl: f64,
    pub notional_usd: f64,
}
