//! JSON-RPC 2.0 over stdio — the MCP skill interface.
//!
//! **stdout is reserved for MCP protocol only.** All logging goes to stderr via `tracing`.
//! Never use `println!` — use `tracing::info!` etc.
//!
//! 8 tools exposed:
//! - `agent_register`  — register on startup
//! - `request_credit`  — submit CreditProposal to Banker
//! - `propose_trade`   — submit trade proposal to Guardian
//! - `repay_credit`    — signal repayment, close line
//! - `get_portfolio`   — read portfolio state
//! - `list_proposals`  — recent proposals with guardian results
//! - `get_risk_score`  — current rolling risk score
//! - `get_credit_line` — read active credit line state

use crate::banker::Banker;
use crate::execution::okx_cex::OkxCexExecutor;
use crate::execution::okx_onchain::OkxOnchainExecutor;
use crate::execution::x402 as x402_interceptor;
use crate::guardian::Guardian;
use crate::monitor::Monitor;
use crate::types::{
    CreditProposal, DashboardEvent, JsonRpcRequest, JsonRpcResponse, McpManifest, McpTool,
    PolicyConfig, RepaymentTrigger, TradeProposal, TradeSide, X402PaymentRequest,
};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Build the MCP tool manifest advertising our 8 tools.
pub fn build_manifest() -> McpManifest {
    McpManifest {
        name: "openclaw-aibank".to_string(),
        version: "0.1.0".to_string(),
        description: "Supervised agentic trading — borrow, trade, repay".to_string(),
        tools: vec![
            McpTool {
                name: "agent_register".to_string(),
                description: "Register an agent. No trades possible without registration."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Agent display name" },
                        "evm_address": { "type": "string", "description": "Optional EVM address (0x-prefixed) for on-chain treasury credit grants" }
                    },
                    "required": ["name"]
                }),
            },
            McpTool {
                name: "request_credit".to_string(),
                description: "Submit a CreditProposal to the Banker for scoring and approval."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "UUID of registered agent" },
                        "requested_usd": { "type": "number" },
                        "max_loss_usd": { "type": "number" },
                        "target_return_pct": { "type": "number" },
                        "window_start": { "type": "string", "format": "date-time" },
                        "window_end": { "type": "string", "format": "date-time" },
                        "strategy": { "type": "string" },
                        "allowed_pairs": { "type": "array", "items": { "type": "string" } },
                        "max_single_trade_usd": { "type": "number" },
                        "repayment_trigger": { "type": "string", "enum": ["profit_target", "stop_loss", "time_expiry", "manual"] },
                        "collateral_asset": { "type": "string" },
                        "collateral_amount": { "type": "number" }
                    },
                    "required": ["agent_id", "requested_usd", "max_loss_usd", "target_return_pct", "window_end", "strategy", "allowed_pairs", "max_single_trade_usd"]
                }),
            },
            McpTool {
                name: "propose_trade".to_string(),
                description: "Submit a trade proposal. Runs through Guardian before execution."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string" },
                        "pair": { "type": "string" },
                        "side": { "type": "string", "enum": ["buy", "sell"] },
                        "amount_usd": { "type": "number" },
                        "confidence": { "type": "number" },
                        "reasoning": { "type": "string" },
                        "contract_address": { "type": "string" },
                        "contract_method": { "type": "string" }
                    },
                    "required": ["agent_id", "pair", "side", "amount_usd", "confidence", "reasoning"]
                }),
            },
            McpTool {
                name: "repay_credit".to_string(),
                description: "Signal repayment. Closes the active credit line.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string" }
                    },
                    "required": ["agent_id"]
                }),
            },
            McpTool {
                name: "get_portfolio".to_string(),
                description: "Read current portfolio balances.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "list_proposals".to_string(),
                description: "List recent trade proposals with guardian results.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "default": 20 }
                    }
                }),
            },
            McpTool {
                name: "get_risk_score".to_string(),
                description: "Get the current rolling risk score for an agent.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string" }
                    },
                    "required": ["agent_id"]
                }),
            },
            McpTool {
                name: "get_credit_line".to_string(),
                description: "Read the active credit line state for an agent.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string" }
                    },
                    "required": ["agent_id"]
                }),
            },
            McpTool {
                name: "submit_x402_payment".to_string(),
                description: "Submit an x402 payment for guardian review. Suspicious payments require human approval.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string" },
                        "recipient": { "type": "string", "description": "Recipient address (0x...)" },
                        "amount_usd": { "type": "number" },
                        "currency": { "type": "string", "default": "USDC" },
                        "service_url": { "type": "string", "description": "URL of the service requesting payment" },
                        "purpose": { "type": "string", "description": "What the payment is for" }
                    },
                    "required": ["agent_id", "recipient", "amount_usd", "service_url", "purpose"]
                }),
            },
        ],
    }
}

/// Run the MCP stdio loop. Reads JSON-RPC requests from stdin, writes responses to stdout.
///
/// **stdout must NEVER be contaminated with anything other than JSON-RPC responses.**
pub async fn run_stdio_loop(
    banker: Arc<Banker>,
    guardian: Arc<Guardian>,
    monitor: Arc<Monitor>,
    cex_executor: Arc<OkxCexExecutor>,
    onchain_executor: Arc<OkxOnchainExecutor>,
    tx: broadcast::Sender<DashboardEvent>,
) {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    info!("MCP stdio loop started — awaiting JSON-RPC requests");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to parse JSON-RPC request: {e}");
                let response = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    -32700,
                    format!("Parse error: {e}"),
                );
                let _ = write_response(&mut stdout, &response).await;
                continue;
            }
        };

        info!(method = %request.method, id = %request.id, "Received MCP request");

        let response = handle_request(
            &request,
            &banker,
            &guardian,
            &monitor,
            &cex_executor,
            &onchain_executor,
            &tx,
        )
        .await;

        if let Err(e) = write_response(&mut stdout, &response).await {
            error!("Failed to write response: {e}");
        }
    }

    info!("MCP stdio loop ended — stdin closed");
}

/// Write a JSON-RPC response to stdout, followed by a newline.
async fn write_response(
    stdout: &mut io::Stdout,
    response: &JsonRpcResponse,
) -> Result<(), std::io::Error> {
    let json = serde_json::to_string(response)
        .map_err(|e| std::io::Error::other(format!("Serialize error: {e}")))?;
    stdout.write_all(json.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

/// Route a request to the appropriate handler.
async fn handle_request(
    request: &JsonRpcRequest,
    banker: &Arc<Banker>,
    guardian: &Arc<Guardian>,
    monitor: &Arc<Monitor>,
    cex_executor: &Arc<OkxCexExecutor>,
    onchain_executor: &Arc<OkxOnchainExecutor>,
    tx: &broadcast::Sender<DashboardEvent>,
) -> JsonRpcResponse {
    match request.method.as_str() {
        // MCP capability discovery
        "initialize" | "tools/list" => {
            let manifest = build_manifest();
            JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(&manifest).unwrap_or_default(),
            )
        }

        "tools/call" => {
            let tool_name = request.params.get("name").and_then(|v| v.as_str());
            let arguments = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            match tool_name {
                Some("agent_register") => handle_agent_register(request, banker, &arguments).await,
                Some("request_credit") => handle_request_credit(request, banker, &arguments).await,
                Some("propose_trade") => {
                    handle_propose_trade(
                        request,
                        banker,
                        guardian,
                        monitor,
                        cex_executor,
                        onchain_executor,
                        tx,
                        &arguments,
                    )
                    .await
                }
                Some("repay_credit") => handle_repay_credit(request, banker, &arguments).await,
                Some("get_portfolio") => handle_get_portfolio(request, monitor).await,
                Some("list_proposals") => handle_list_proposals(request, monitor).await,
                Some("get_risk_score") => handle_get_risk_score(request, banker, &arguments).await,
                Some("get_credit_line") => {
                    handle_get_credit_line(request, banker, &arguments).await
                }
                Some("submit_x402_payment") => {
                    handle_submit_x402(request, banker, &arguments).await
                }
                Some(name) => JsonRpcResponse::error(
                    request.id.clone(),
                    -32601,
                    format!("Unknown tool: {name}"),
                ),
                None => JsonRpcResponse::error(
                    request.id.clone(),
                    -32602,
                    "Missing tool name in params".to_string(),
                ),
            }
        }

        method => JsonRpcResponse::error(
            request.id.clone(),
            -32601,
            format!("Method not found: {method}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

async fn handle_agent_register(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                "Missing required parameter: name".to_string(),
            );
        }
    };

    let evm_address = args
        .get("evm_address")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let agent = banker.register_agent(name, evm_address).await;
    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&agent).unwrap_or_default()
            }]
        }),
    )
}

async fn handle_request_credit(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let agent_id = match parse_uuid(args, "agent_id") {
        Ok(id) => id,
        Err(e) => return JsonRpcResponse::error(req.id.clone(), -32602, e),
    };

    if !banker.is_registered(agent_id).await {
        return JsonRpcResponse::error(
            req.id.clone(),
            -32602,
            format!("Agent {agent_id} is not registered"),
        );
    }

    let requested_usd = args
        .get("requested_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let max_loss_usd = args
        .get("max_loss_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let target_return_pct = args
        .get("target_return_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let strategy = args
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let allowed_pairs: Vec<String> = args
        .get("allowed_pairs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let max_single_trade_usd = args
        .get("max_single_trade_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let window_start = args
        .get("window_start")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now);

    let window_end = match args
        .get("window_end")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
    {
        Some(t) => t,
        None => {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                "Missing or invalid window_end".to_string(),
            );
        }
    };

    let repayment_trigger = match args
        .get("repayment_trigger")
        .and_then(|v| v.as_str())
        .unwrap_or("manual")
    {
        "profit_target" => RepaymentTrigger::ProfitTarget {
            pct: target_return_pct,
        },
        "stop_loss" => RepaymentTrigger::StopLoss {
            loss_usd: max_loss_usd,
        },
        "time_expiry" => RepaymentTrigger::TimeExpiry,
        _ => RepaymentTrigger::Manual,
    };

    let collateral = args
        .get("collateral_asset")
        .and_then(|v| v.as_str())
        .map(|asset| crate::types::Collateral {
            asset: asset.to_string(),
            amount: args
                .get("collateral_amount")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            locked_at: Utc::now(),
        });

    let proposal = CreditProposal {
        id: Uuid::new_v4(),
        agent_id,
        submitted_at: Utc::now(),
        requested_usd,
        max_loss_usd,
        target_return_pct,
        window_start,
        window_end,
        strategy,
        allowed_pairs,
        max_single_trade_usd,
        repayment_trigger,
        collateral,
    };

    let decision = banker.evaluate(&proposal).await;

    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&decision).unwrap_or_default()
            }]
        }),
    )
}

#[allow(clippy::too_many_arguments)]
async fn handle_propose_trade(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    guardian: &Arc<Guardian>,
    monitor: &Arc<Monitor>,
    cex_executor: &Arc<OkxCexExecutor>,
    onchain_executor: &Arc<OkxOnchainExecutor>,
    tx: &broadcast::Sender<DashboardEvent>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let agent_id = match parse_uuid(args, "agent_id") {
        Ok(id) => id,
        Err(e) => return JsonRpcResponse::error(req.id.clone(), -32602, e),
    };

    if !banker.is_registered(agent_id).await {
        return JsonRpcResponse::error(
            req.id.clone(),
            -32602,
            format!("Agent {agent_id} is not registered"),
        );
    }

    let pair = args
        .get("pair")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let side = match args.get("side").and_then(|v| v.as_str()) {
        Some("buy") => TradeSide::Buy,
        Some("sell") => TradeSide::Sell,
        _ => {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                "Invalid or missing side (must be 'buy' or 'sell')".to_string(),
            );
        }
    };
    let amount_usd = args
        .get("amount_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .min(1.0); // Hard cap: never exceed $1 per trade
    let confidence = args
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let reasoning = args
        .get("reasoning")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let contract_address = args
        .get("contract_address")
        .and_then(|v| v.as_str())
        .map(String::from);
    let contract_method = args
        .get("contract_method")
        .and_then(|v| v.as_str())
        .map(String::from);

    let proposal = TradeProposal {
        id: Uuid::new_v4(),
        agent_id,
        submitted_at: Utc::now(),
        pair: pair.clone(),
        side,
        amount_usd,
        confidence,
        reasoning,
        contract_address: contract_address.clone(),
        contract_method,
    };

    // Record the proposal
    let _ = tx.send(DashboardEvent::ProposalSubmitted {
        proposal: proposal.clone(),
    });
    monitor.record_proposal(proposal.clone()).await;

    // Guardian verification — ALL checks must pass
    let guardian_result = match guardian.verify(&proposal).await {
        Ok(r) => r,
        Err(e) => {
            return JsonRpcResponse::error(req.id.clone(), -32000, format!("Guardian error: {e}"));
        }
    };

    monitor
        .record_guardian_result(guardian_result.clone())
        .await;

    if !guardian_result.approved {
        let reasons: Vec<String> = guardian_result
            .checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect();

        let _ = tx.send(DashboardEvent::TradeRejected {
            proposal_id: proposal.id,
            agent_id,
            reason: reasons.join("; "),
        });

        return JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&serde_json::json!({
                        "approved": false,
                        "guardian_result": guardian_result,
                        "failed_checks": reasons,
                    })).unwrap_or_default()
                }]
            }),
        );
    }

    // Deduct from credit line (buys only — sells reconvert to USDT, no spend)
    let is_sell = proposal.side == TradeSide::Sell;
    if !is_sell {
        if let Err(e) = banker.deduct(agent_id, amount_usd).await {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32000,
                format!("Credit deduction failed: {e}"),
            );
        }
    }

    // Execute trade via appropriate executor
    let execution_result = if contract_address.is_some() {
        onchain_executor.execute(&proposal).await
    } else {
        cex_executor.execute(&proposal).await
    };

    match execution_result {
        Ok(result) => {
            let _ = tx.send(DashboardEvent::TradeExecuted {
                proposal_id: proposal.id,
                agent_id,
                pair,
                side: proposal.side.clone(),
                amount_usd,
            });

            JsonRpcResponse::success(
                req.id.clone(),
                serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string(&serde_json::json!({
                            "approved": true,
                            "guardian_result": guardian_result,
                            "execution": result,
                        })).unwrap_or_default()
                    }]
                }),
            )
        }
        Err(e) => {
            warn!(proposal_id = %proposal.id, error = %e, "Trade execution failed");
            // Refund the deducted amount so the agent doesn't lose budget (buys only)
            if !is_sell {
                if let Err(refund_err) = banker.refund(agent_id, amount_usd).await {
                    error!(agent_id = %agent_id, error = %refund_err, "Failed to refund credit after execution failure");
                }
            }
            JsonRpcResponse::error(req.id.clone(), -32000, format!("Execution failed: {e}"))
        }
    }
}

async fn handle_repay_credit(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let agent_id = match parse_uuid(args, "agent_id") {
        Ok(id) => id,
        Err(e) => return JsonRpcResponse::error(req.id.clone(), -32602, e),
    };

    match banker.repay(agent_id).await {
        Ok(()) => JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Credit line repaid for agent {agent_id}")
                }]
            }),
        ),
        Err(e) => JsonRpcResponse::error(req.id.clone(), -32000, format!("Repay failed: {e}")),
    }
}

async fn handle_get_portfolio(req: &JsonRpcRequest, monitor: &Arc<Monitor>) -> JsonRpcResponse {
    let portfolio = monitor.get_portfolio().await;
    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&portfolio).unwrap_or_default()
            }]
        }),
    )
}

async fn handle_list_proposals(req: &JsonRpcRequest, monitor: &Arc<Monitor>) -> JsonRpcResponse {
    let snapshot = monitor
        .snapshot(Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
        .await;

    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&serde_json::json!({
                    "proposals": snapshot.recent_proposals,
                    "guardian_results": snapshot.recent_guardian_results,
                })).unwrap_or_default()
            }]
        }),
    )
}

async fn handle_get_risk_score(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let agent_id = match parse_uuid(args, "agent_id") {
        Ok(id) => id,
        Err(e) => return JsonRpcResponse::error(req.id.clone(), -32602, e),
    };

    let reputation = banker.reputation(agent_id).await;
    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&reputation).unwrap_or_default()
            }]
        }),
    )
}

async fn handle_get_credit_line(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let agent_id = match parse_uuid(args, "agent_id") {
        Ok(id) => id,
        Err(e) => return JsonRpcResponse::error(req.id.clone(), -32602, e),
    };

    match banker.get_active_line(agent_id).await {
        Some(line) => JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&line).unwrap_or_default()
                }]
            }),
        ),
        None => JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("No active credit line for agent {agent_id}")
                }]
            }),
        ),
    }
}

async fn handle_submit_x402(
    req: &JsonRpcRequest,
    banker: &Arc<Banker>,
    args: &serde_json::Value,
) -> JsonRpcResponse {
    let agent_id = match parse_uuid(args, "agent_id") {
        Ok(id) => id,
        Err(e) => return JsonRpcResponse::error(req.id.clone(), -32602, e),
    };

    if !banker.is_registered(agent_id).await {
        return JsonRpcResponse::error(
            req.id.clone(),
            -32602,
            format!("Agent {agent_id} is not registered"),
        );
    }

    let recipient = args
        .get("recipient")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let amount_usd = args
        .get("amount_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .min(1.0); // Hard cap: same as trades
    let currency = args
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USDC")
        .to_string();
    let service_url = args
        .get("service_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let purpose = args
        .get("purpose")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let payment = X402PaymentRequest {
        id: Uuid::new_v4(),
        agent_id,
        recipient,
        amount_usd,
        currency,
        service_url,
        purpose,
        submitted_at: Utc::now(),
    };

    let credit_line = banker.get_active_line(agent_id).await;
    let policy = PolicyConfig::default();

    let verdict =
        match x402_interceptor::intercept_x402(&payment, credit_line.as_ref(), &policy).await {
            Ok(v) => v,
            Err(e) => {
                return JsonRpcResponse::error(
                    req.id.clone(),
                    -32000,
                    format!("x402 interception error: {e}"),
                );
            }
        };

    if verdict.approved {
        // Low risk: auto-approve and deduct
        if let Err(e) = banker.deduct(agent_id, amount_usd).await {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32000,
                format!("x402 budget deduction failed: {e}"),
            );
        }

        let _ = banker.tx_ref().send(DashboardEvent::X402PaymentApproved {
            payment_id: payment.id,
            agent_id,
        });

        JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&serde_json::json!({
                        "approved": true,
                        "payment_id": payment.id,
                        "risk_level": verdict.risk_level,
                        "reason": verdict.reason,
                    })).unwrap_or_default()
                }]
            }),
        )
    } else if verdict.needs_human_review {
        // Medium risk: queue for human review
        banker
            .store_pending_x402(
                payment.clone(),
                verdict.risk_level.clone(),
                verdict.reason.clone(),
            )
            .await;

        JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&serde_json::json!({
                        "approved": false,
                        "pending_review": true,
                        "payment_id": payment.id,
                        "risk_level": verdict.risk_level,
                        "reason": verdict.reason,
                    })).unwrap_or_default()
                }]
            }),
        )
    } else {
        // High risk: auto-block
        let _ = banker.tx_ref().send(DashboardEvent::X402PaymentBlocked {
            payment_id: payment.id,
            agent_id,
            reason: verdict.reason.clone(),
        });

        JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&serde_json::json!({
                        "approved": false,
                        "blocked": true,
                        "payment_id": payment.id,
                        "risk_level": verdict.risk_level,
                        "reason": verdict.reason,
                    })).unwrap_or_default()
                }]
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a UUID from a JSON value.
fn parse_uuid(args: &serde_json::Value, field: &str) -> Result<Uuid, String> {
    let s = args
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required parameter: {field}"))?;

    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID for {field}: {e}"))
}
