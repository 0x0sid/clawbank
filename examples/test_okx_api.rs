//! Quick OKX API connectivity test.
//! Run with: cargo run --example test_okx_api

use openclaw_aibank::execution::okx_rest::OkxCredentials;

use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
const OKX_BASE_URL: &str = "https://www.okx.com";

#[tokio::main]
async fn main() {
    // Load .env if present
    let _ = dotenvy::dotenv();

    println!("=== OKX API Connectivity Test ===\n");

    // 1. Check credentials
    let creds = match OkxCredentials::from_env() {
        Some(c) => {
            println!("[OK] Credentials loaded");
            println!("     API Key: {}...{}", &c.api_key[..6], &c.api_key[c.api_key.len()-4..]);
            println!("     Passphrase: {} chars", c.passphrase.len());
            c
        }
        None => {
            println!("[FAIL] Missing credentials. Set OKX_API_KEY, OKX_SECRET_KEY, OKX_PASSPHRASE");
            std::process::exit(1);
        }
    };

    let client = Client::new();

    // 2. Test /api/v5/account/balance (requires Read permission)
    println!("\n--- GET /api/v5/account/balance ---");
    match call_api(&client, &creds, "GET", "/api/v5/account/balance", "").await {
        Ok(body) => {
            let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("?");
            let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
            if code == "0" {
                println!("[OK] Balance API works (code=0)");
                if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                    for account in data {
                        if let Some(details) = account.get("details").and_then(|d| d.as_array()) {
                            for detail in details {
                                let ccy = detail.get("ccy").and_then(|v| v.as_str()).unwrap_or("?");
                                let eq = detail.get("eq").and_then(|v| v.as_str()).unwrap_or("0");
                                let avail = detail.get("availBal").and_then(|v| v.as_str()).unwrap_or("0");
                                println!("     {ccy}: eq={eq}, available={avail}");
                            }
                            if details.is_empty() {
                                println!("     (no balances — account may be empty)");
                            }
                        }
                    }
                }
            } else {
                println!("[FAIL] code={code}, msg={msg}");
                println!("     Full response: {body}");
            }
        }
        Err(e) => println!("[FAIL] {e}"),
    }

    // 3. Test /api/v5/account/positions (requires Read permission)
    println!("\n--- GET /api/v5/account/positions ---");
    match call_api(&client, &creds, "GET", "/api/v5/account/positions", "").await {
        Ok(body) => {
            let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("?");
            let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
            if code == "0" {
                let count = body.get("data").and_then(|d| d.as_array()).map(|a| a.len()).unwrap_or(0);
                println!("[OK] Positions API works (code=0, {count} open positions)");
            } else {
                println!("[FAIL] code={code}, msg={msg}");
            }
        }
        Err(e) => println!("[FAIL] {e}"),
    }

    // 4. Test /api/v5/account/config (account configuration — good permission check)
    println!("\n--- GET /api/v5/account/config ---");
    match call_api(&client, &creds, "GET", "/api/v5/account/config", "").await {
        Ok(body) => {
            let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("?");
            let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
            if code == "0" {
                println!("[OK] Account config API works (code=0)");
                if let Some(data) = body.get("data").and_then(|d| d.as_array()).and_then(|a| a.first()) {
                    let uid = data.get("uid").and_then(|v| v.as_str()).unwrap_or("?");
                    let acct_level = data.get("acctLv").and_then(|v| v.as_str()).unwrap_or("?");
                    let perm = data.get("perm").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("     UID: {uid}");
                    println!("     Account level: {acct_level}");
                    println!("     Permissions: {perm}");
                    if perm.contains("trade") {
                        println!("     [OK] Trade permission ENABLED");
                    } else {
                        println!("     [WARN] Trade permission NOT enabled — live trades will fail");
                    }
                }
            } else {
                println!("[FAIL] code={code}, msg={msg}");
            }
        }
        Err(e) => println!("[FAIL] {e}"),
    }

    println!("\n=== Done ===");
}

async fn call_api(
    client: &Client,
    creds: &OkxCredentials,
    method: &str,
    path: &str,
    body: &str,
) -> Result<serde_json::Value, String> {
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let prehash = format!("{timestamp}{method}{path}{body}");
    let mut mac = HmacSha256::new_from_slice(creds.secret_key.as_bytes())
        .map_err(|e| format!("HMAC error: {e}"))?;
    mac.update(prehash.as_bytes());
    let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    let resp = client
        .get(format!("{OKX_BASE_URL}{path}"))
        .header("OK-ACCESS-KEY", &creds.api_key)
        .header("OK-ACCESS-SIGN", &sign)
        .header("OK-ACCESS-TIMESTAMP", &timestamp)
        .header("OK-ACCESS-PASSPHRASE", &creds.passphrase)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;

    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("Parse failed: {e}"))
}
