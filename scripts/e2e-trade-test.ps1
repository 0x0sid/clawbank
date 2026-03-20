# =============================================================================
# E2E Trade Test — Full MCP flow with Banker + Guardian notifications
# =============================================================================
# Starts clawbot, sends MCP JSON-RPC messages, displays Banker/Guardian output.
# Dashboard at http://localhost:3030 shows live events.
#
# Usage: .\scripts\e2e-trade-test.ps1
# =============================================================================

$ErrorActionPreference = "Continue"
$binary = (Resolve-Path ".\target\release\openclaw-aibank.exe").Path

# Load .env
if (Test-Path .env) {
    Get-Content .env | ForEach-Object {
        if ($_ -match '^([^#][^=]+)=(.+)$') {
            [System.Environment]::SetEnvironmentVariable($matches[1].Trim(), $matches[2].Trim(), 'Process')
        }
    }
}
$env:RUST_LOG = "info"

# Kill any lingering clawbot process
Get-Process -Name "openclaw-aibank" -ErrorAction SilentlyContinue | Stop-Process -Force 2>$null
Start-Sleep -Seconds 1

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  OpenClaw AI Bank - E2E Trade Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Dashboard: http://localhost:3030" -ForegroundColor Yellow
Write-Host ""

# --- Start a SINGLE long-lived process ---
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $binary
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true

# Pass env vars to the child process
foreach ($key in @("OKX_API_KEY","OKX_SECRET_KEY","OKX_PASSPHRASE","OKX_ONCHAIN_API_KEY","BANKER_KEY","TREASURY_ADDRESS","TREASURY_RPC_URL","DASHBOARD_PORT","RUST_LOG")) {
    $val = [System.Environment]::GetEnvironmentVariable($key, 'Process')
    if ($val) {
        $psi.EnvironmentVariables[$key] = $val
    }
}

$proc = New-Object System.Diagnostics.Process
$proc.StartInfo = $psi

# Collect stderr asynchronously using events
$stderrLines = [System.Collections.ArrayList]::Synchronized([System.Collections.ArrayList]::new())
$stderrAction = {
    if ($EventArgs.Data) {
        $stderrLines = $Event.MessageData
        [void]$stderrLines.Add($EventArgs.Data)
    }
}
Register-ObjectEvent -InputObject $proc -EventName ErrorDataReceived -Action $stderrAction -MessageData $stderrLines | Out-Null

$proc.Start() | Out-Null
$proc.BeginErrorReadLine()

# Give the server time to start (dashboard + poller + MCP loop)
Start-Sleep -Seconds 2

Write-Host "[startup] Server running (PID: $($proc.Id))" -ForegroundColor DarkGray
Write-Host ""

# --- Helper: send one request, read one response line ---
function Send-Mcp {
    param([string]$Label, [string]$Json)

    Write-Host "-------------------------------------------" -ForegroundColor DarkGray
    Write-Host "[$Label]" -ForegroundColor Yellow

    $proc.StandardInput.WriteLine($Json)
    $proc.StandardInput.Flush()

    # Read exactly one line from stdout (one JSON-RPC response per line)
    $line = $proc.StandardOutput.ReadLine()

    # Brief pause to let stderr events fire
    Start-Sleep -Milliseconds 200

    # Show any new stderr logs since last call
    $newLogs = @($stderrLines.ToArray())
    $stderrLines.Clear()
    foreach ($log in $newLogs) {
        $color = "DarkGray"
        if ($log -match "Credit line granted|approved_usd") { $color = "Green" }
        elseif ($log -match "Guardian|checks passed|check_") { $color = "Cyan" }
        elseif ($log -match "rejected|FAIL|Rejected|not registered") { $color = "Red" }
        elseif ($log -match "executed|simulated|filled") { $color = "Magenta" }
        elseif ($log -match "registered|Agent registered") { $color = "Blue" }
        elseif ($log -match "refund") { $color = "Yellow" }
        elseif ($log -match "deduct|spent") { $color = "DarkYellow" }
        elseif ($log -match "repaid|reputation") { $color = "DarkGreen" }
        Write-Host "  LOG: $log" -ForegroundColor $color
    }

    # Parse and display response
    if ($line) {
        try {
            $resp = $line | ConvertFrom-Json
            if ($resp.error) {
                Write-Host "  >> [ERROR] $($resp.error.message)" -ForegroundColor Red
            }
            elseif ($resp.result.content) {
                foreach ($item in $resp.result.content) {
                    if ($item.text) {
                        try {
                            $inner = $item.text | ConvertFrom-Json

                            if ($inner.approved -eq $true -and $inner.guardian_result) {
                                # Trade approved by Guardian
                                Write-Host "  >> [TRADE APPROVED]" -ForegroundColor Green
                                Write-Host "  Guardian checks:" -ForegroundColor Cyan
                                foreach ($c in $inner.guardian_result.checks) {
                                    $icon = if ($c.passed) { "PASS" } else { "FAIL" }
                                    $cc = if ($c.passed) { "Green" } else { "Red" }
                                    Write-Host "    [$icon] $($c.name): $($c.detail)" -ForegroundColor $cc
                                }
                                if ($inner.execution) {
                                    Write-Host "  Execution:" -ForegroundColor Magenta
                                    Write-Host "    Status: $($inner.execution.status) | Pair: $($inner.execution.pair)" -ForegroundColor White
                                }
                            }
                            elseif ($inner.approved -eq $false -and $inner.guardian_result) {
                                # Trade rejected by Guardian
                                Write-Host "  >> [TRADE REJECTED BY GUARDIAN]" -ForegroundColor Red
                                foreach ($c in $inner.guardian_result.checks) {
                                    $icon = if ($c.passed) { "PASS" } else { "FAIL" }
                                    $cc = if ($c.passed) { "Green" } else { "Red" }
                                    Write-Host "    [$icon] $($c.name): $($c.detail)" -ForegroundColor $cc
                                }
                            }
                            elseif ($inner.approved -ne $null -and $inner.approved_usd) {
                                # Credit decision
                                if ($inner.approved) {
                                    Write-Host "  >> [CREDIT APPROVED] `$$($inner.approved_usd) | Score: $($inner.score)" -ForegroundColor Green
                                } else {
                                    Write-Host "  >> [CREDIT DENIED] Reason: $($inner.rejection_reason)" -ForegroundColor Red
                                }
                            }
                            else {
                                # Generic (credit line, reputation, etc.)
                                Write-Host "  >> $($inner | ConvertTo-Json -Depth 4 -Compress)" -ForegroundColor White
                            }
                        } catch {
                            Write-Host "  >> $($item.text)" -ForegroundColor White
                        }
                    }
                }
            }
        } catch {
            Write-Host "  >> Raw: $line" -ForegroundColor Gray
        }
    }
    Write-Host ""
    return $line
}

# =================== STEP 1: Register Agent ===================
$resp = Send-Mcp "STEP 1: Register Agent" '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"agent_register","arguments":{"name":"claw-trader-01"}}}'

# Extract agent_id
$agentId = $null
try {
    $parsed = $resp | ConvertFrom-Json
    $innerText = ($parsed.result.content | Where-Object { $_.type -eq "text" }).text
    $inner = $innerText | ConvertFrom-Json
    $agentId = $inner.id
    Write-Host "  Agent ID: $agentId" -ForegroundColor Cyan
} catch {
    Write-Host "  [FATAL] Could not parse agent_id" -ForegroundColor Red
    $proc.StandardInput.Close()
    $proc.Kill()
    exit 1
}

# =================== STEP 2: Request Credit Line ===================
$windowEnd = (Get-Date).AddHours(2).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.fffZ")

$creditReq = @{
    jsonrpc = "2.0"; id = 2; method = "tools/call"
    params = @{
        name = "request_credit"
        arguments = @{
            agent_id = $agentId; requested_usd = 10; max_loss_usd = 5
            target_return_pct = 5.0
            strategy = "Momentum strategy on BTC and ETH using 4H RSI crossover with volume confirmation and trailing stop"
            allowed_pairs = @("BTC-USDT", "ETH-USDT")
            max_single_trade_usd = 5; window_end = $windowEnd
            repayment_trigger = "manual"
            collateral_asset = "USDT"; collateral_amount = 5
        }
    }
} | ConvertTo-Json -Depth 5 -Compress

Send-Mcp "STEP 2: Request Credit (Banker decides)" $creditReq | Out-Null

# =================== STEP 3: Good Trade ===================
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  Guardian Trade Checks" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""

$goodTrade = @{
    jsonrpc = "2.0"; id = 3; method = "tools/call"
    params = @{
        name = "propose_trade"
        arguments = @{
            agent_id = $agentId; pair = "BTC-USDT"; side = "buy"
            amount_usd = 1; confidence = 0.85
            reasoning = "BTC strong momentum on 4H, RSI breakout above 70 with volume confirmation"
        }
    }
} | ConvertTo-Json -Depth 5 -Compress

Send-Mcp "STEP 3: LIVE Trade - BTC-USDT Buy `$1 (confidence 0.85)" $goodTrade | Out-Null

# =================== STEP 4: Bad Trade ===================
$badTrade = @{
    jsonrpc = "2.0"; id = 4; method = "tools/call"
    params = @{
        name = "propose_trade"
        arguments = @{
            agent_id = $agentId; pair = "DOGE-USDT"; side = "buy"
            amount_usd = 3000; confidence = 0.3
            reasoning = "yolo moon shot"
        }
    }
} | ConvertTo-Json -Depth 5 -Compress

Send-Mcp "STEP 4: Bad Trade - DOGE-USDT Buy `$3000 (confidence 0.3)" $badTrade | Out-Null

# =================== STEP 5: Check Credit Line ===================
$check = @{
    jsonrpc = "2.0"; id = 5; method = "tools/call"
    params = @{ name = "get_credit_line"; arguments = @{ agent_id = $agentId } }
} | ConvertTo-Json -Depth 5 -Compress

Send-Mcp "STEP 5: Credit Line Status (budget remaining)" $check | Out-Null

# =================== STEP 6: Check Reputation ===================
$rep = @{
    jsonrpc = "2.0"; id = 6; method = "tools/call"
    params = @{ name = "get_risk_score"; arguments = @{ agent_id = $agentId } }
} | ConvertTo-Json -Depth 5 -Compress

Send-Mcp "STEP 6: Agent Reputation" $rep | Out-Null

# =================== STEP 7: Repay ===================
$repay = @{
    jsonrpc = "2.0"; id = 7; method = "tools/call"
    params = @{ name = "repay_credit"; arguments = @{ agent_id = $agentId } }
} | ConvertTo-Json -Depth 5 -Compress

Send-Mcp "STEP 7: Repay Credit (close line)" $repay | Out-Null

# =================== DONE ===================
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  E2E TEST COMPLETE" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "  All 7 steps executed against a single live process." -ForegroundColor White
Write-Host "  Dashboard was live at http://localhost:3030" -ForegroundColor Yellow
Write-Host ""

# Cleanup
$proc.StandardInput.Close()
Start-Sleep -Milliseconds 500
if (-not $proc.HasExited) { $proc.Kill() }
Get-EventSubscriber | Unregister-Event -Force 2>$null
Write-Host "[done] Server stopped." -ForegroundColor DarkGray
