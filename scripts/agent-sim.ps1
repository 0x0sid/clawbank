#!/usr/bin/env pwsh
# =============================================================================
# OpenClaw AI Agent Trading Simulation
# =============================================================================
# Starts the server, registers an agent, submits a credit proposal,
# waits for human approval on the dashboard, then trades BTC<->USDT
# back and forth in a loop.
#
# Usage: powershell -ExecutionPolicy Bypass -File .\scripts\agent-sim.ps1
# Then open http://localhost:3030 and approve the credit proposal.
# =============================================================================

$ErrorActionPreference = "Continue"

# Load .env
if (Test-Path ".\.env") {
    Get-Content ".\.env" | ForEach-Object {
        if ($_ -match '^\s*([^#][^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1].Trim(), $Matches[2].Trim(), "Process")
        }
    }
}

# Kill lingering processes
Get-Process -Name "openclaw-aibank" -ErrorAction SilentlyContinue | Stop-Process -Force 2>$null
Start-Sleep -Seconds 1

# ---- Start server as a .NET Process with redirected IO ----
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path ".\target\release\openclaw-aibank.exe").Path
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true

# Copy env vars
foreach ($key in @("OKX_API_KEY","OKX_SECRET_KEY","OKX_PASSPHRASE","BANKER_KEY","TREASURY_ADDRESS","TREASURY_RPC_URL","RUST_LOG")) {
    $val = [Environment]::GetEnvironmentVariable($key, "Process")
    if ($val) { $psi.EnvironmentVariables[$key] = $val }
}

$proc = New-Object System.Diagnostics.Process
$proc.StartInfo = $psi

# Collect stderr in background
$stderrBuf = New-Object System.Text.StringBuilder
$stderrAction = {
    if ($EventArgs.Data) { $Event.MessageData.AppendLine($EventArgs.Data) | Out-Null }
}
Register-ObjectEvent -InputObject $proc -EventName ErrorDataReceived -Action $stderrAction -MessageData $stderrBuf | Out-Null

$proc.Start() | Out-Null
$proc.BeginErrorReadLine()

Start-Sleep -Seconds 2

function Flush-Stderr {
    $text = $stderrBuf.ToString()
    if ($text.Length -gt 0) {
        $stderrBuf.Clear() | Out-Null
        $text -split "`n" | Where-Object { $_.Trim() } | ForEach-Object {
            $line = $_.Trim()
            if ($line -match "approved|granted") { Write-Host "  LOG: $line" -ForegroundColor Green }
            elseif ($line -match "rejected|WARN|error") { Write-Host "  LOG: $line" -ForegroundColor Yellow }
            else { Write-Host "  LOG: $line" -ForegroundColor DarkGray }
        }
    }
}

function Send-Mcp {
    param([string]$Json)
    $proc.StandardInput.WriteLine($Json)
    $proc.StandardInput.Flush()
    Start-Sleep -Milliseconds 200
    $line = $proc.StandardOutput.ReadLine()
    Flush-Stderr
    return $line
}

function Parse-Content {
    param([string]$Raw)
    try {
        $resp = $Raw | ConvertFrom-Json
        if ($resp.result -and $resp.result.content) {
            $text = $resp.result.content[0].text
            return ($text | ConvertFrom-Json)
        }
        if ($resp.error) { return $resp.error }
    } catch { return $null }
    return $null
}

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host "  OpenClaw AI Agent Trading Simulation" -ForegroundColor Cyan
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host "  Dashboard: http://localhost:3030" -ForegroundColor White
Write-Host "  Server PID: $($proc.Id)" -ForegroundColor DarkGray
Write-Host ""

# ---- STEP 1: Register Agent ----
Write-Host "[1/3] Registering agent..." -ForegroundColor Cyan
$regReq = @{
    jsonrpc = "2.0"; id = 1; method = "tools/call"
    params = @{ name = "agent_register"; arguments = @{ name = "claw-trader-01"; evm_address = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8" } }
} | ConvertTo-Json -Depth 5 -Compress

$regResp = Send-Mcp $regReq
$regData = Parse-Content $regResp
$agentId = $regData.id
Write-Host "  Agent registered: $agentId" -ForegroundColor Green
Write-Host ""

# ---- STEP 2: Submit Credit Proposal ----
Write-Host "[2/3] Submitting credit proposal for human approval..." -ForegroundColor Cyan
$windowEnd = (Get-Date).AddHours(2).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.fffZ")

$creditReq = @{
    jsonrpc = "2.0"; id = 2; method = "tools/call"
    params = @{
        name = "request_credit"
        arguments = @{
            agent_id = $agentId; requested_usd = 1; max_loss_usd = 0.5
            target_return_pct = 2.0
            strategy = "Round-trip momentum trading BTC-USDT using 4H RSI crossover with volume confirmation and trailing stop-loss. Buy 1 USD BTC then sell back to USDT."
            allowed_pairs = @("BTC-USDT")
            max_single_trade_usd = 1; window_end = $windowEnd
            repayment_trigger = "manual"
            collateral_asset = "USDT"; collateral_amount = 1
        }
    }
} | ConvertTo-Json -Depth 5 -Compress

$creditResp = Send-Mcp $creditReq
$creditData = Parse-Content $creditResp
Write-Host "  Score: $($creditData.score)" -ForegroundColor Yellow
Write-Host "  Recommended: `$$($creditData.approved_usd)" -ForegroundColor Yellow
Write-Host ""
Write-Host "  >>>  GO TO http://localhost:3030 AND CLICK APPROVE  <<<" -ForegroundColor White -BackgroundColor DarkGreen
Write-Host ""

# ---- STEP 3: Poll for credit line until approved ----
Write-Host "[3/3] Waiting for credit approval on dashboard..." -ForegroundColor Cyan
$idCounter = 3
$creditApproved = $false

for ($i = 0; $i -lt 120; $i++) {
    Start-Sleep -Seconds 2
    $checkReq = @{
        jsonrpc = "2.0"; id = $idCounter; method = "tools/call"
        params = @{ name = "get_credit_line"; arguments = @{ agent_id = $agentId } }
    } | ConvertTo-Json -Depth 5 -Compress
    $idCounter++

    $checkResp = Send-Mcp $checkReq
    $checkData = Parse-Content $checkResp

    if ($checkData -and $checkData.id -and $checkData.status -eq "Active") {
        $creditApproved = $true
        Write-Host "  Credit APPROVED! Budget: `$$($checkData.approved_usd)" -ForegroundColor Green
        break
    }

    if ($i % 5 -eq 0) {
        Write-Host "  ... still waiting (${i}s) - approve on dashboard" -ForegroundColor DarkGray
    }
}

if (-not $creditApproved) {
    Write-Host "  Timed out waiting for approval. Exiting." -ForegroundColor Red
    $proc.Kill()
    exit 1
}

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host "  Starting BTC <-> USDT Round-Trip Trades" -ForegroundColor Cyan
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host ""

# ---- TRADING LOOP: BUY $1 BTC then SELL back to USDT ----
$tradeCount = 0

# With $1 budget: 1 buy uses the full budget, then sell it back (free — no budget deduction)
# BUY: Convert $1 USDT -> BTC
$tradeCount++
Write-Host "  Trade #$tradeCount | BUY `$1 BTC-USDT (USDT -> BTC)" -ForegroundColor Green

$buyReq = @{
    jsonrpc = "2.0"; id = $idCounter; method = "tools/call"
    params = @{
        name = "propose_trade"
        arguments = @{
            agent_id = $agentId; pair = "BTC-USDT"; side = "buy"
            amount_usd = 1; confidence = 0.82
            reasoning = "BTC dip on 15m chart, RSI oversold at 28, volume spike confirms reversal"
        }
    }
} | ConvertTo-Json -Depth 5 -Compress
$idCounter++

$buyResp = Send-Mcp $buyReq
$buyData = Parse-Content $buyResp
$buyOk = $false

if ($buyData.guardian_result -and $buyData.guardian_result.approved) {
    $isLive = if ($buyData.execution -and $buyData.execution.live) { "LIVE" } else { "SIM" }
    Write-Host "    -> APPROVED [$isLive] status=$($buyData.execution.status)" -ForegroundColor Green
    if ($buyData.execution -and $buyData.execution.order_id) {
        Write-Host "    -> OKX Order: $($buyData.execution.order_id)" -ForegroundColor DarkCyan
    }
    $buyOk = $true
} else {
    $reason = if ($buyData.guardian_result) {
        ($buyData.guardian_result.checks | Where-Object { -not $_.passed } | ForEach-Object { $_.detail }) -join "; "
    } else { "unknown" }
    Write-Host "    -> REJECTED: $reason" -ForegroundColor Red
}
Flush-Stderr
Start-Sleep -Seconds 2

# SELL: Convert BTC back -> USDT (only if buy succeeded)
if ($buyOk) {
    $tradeCount++
    Write-Host "  Trade #$tradeCount | SELL `$1 BTC-USDT (BTC -> USDT)" -ForegroundColor Magenta

    $sellReq = @{
        jsonrpc = "2.0"; id = $idCounter; method = "tools/call"
        params = @{
            name = "propose_trade"
            arguments = @{
                agent_id = $agentId; pair = "BTC-USDT"; side = "sell"
                amount_usd = 1; confidence = 0.78
                reasoning = "Reconverting BTC back to USDT to close position"
            }
        }
    } | ConvertTo-Json -Depth 5 -Compress
    $idCounter++

    $sellResp = Send-Mcp $sellReq
    $sellData = Parse-Content $sellResp

    if ($sellData.guardian_result -and $sellData.guardian_result.approved) {
        $isLive = if ($sellData.execution -and $sellData.execution.live) { "LIVE" } else { "SIM" }
        Write-Host "    -> APPROVED [$isLive] status=$($sellData.execution.status)" -ForegroundColor Green
        if ($sellData.execution -and $sellData.execution.order_id) {
            Write-Host "    -> OKX Order: $($sellData.execution.order_id)" -ForegroundColor DarkCyan
        }
    } else {
        $reason = if ($sellData.guardian_result) {
            ($sellData.guardian_result.checks | Where-Object { -not $_.passed } | ForEach-Object { $_.detail }) -join "; "
        } else { "unknown" }
        Write-Host "    -> REJECTED: $reason" -ForegroundColor Red
    }
    Flush-Stderr
} else {
    Write-Host "  Skipping sell — buy was not executed" -ForegroundColor DarkGray
}

Write-Host ""

# Check final credit status
$statusReq = @{
    jsonrpc = "2.0"; id = $idCounter; method = "tools/call"
    params = @{ name = "get_credit_line"; arguments = @{ agent_id = $agentId } }
} | ConvertTo-Json -Depth 5 -Compress
$idCounter++

$statusResp = Send-Mcp $statusReq
$statusData = Parse-Content $statusResp
if ($statusData -and $statusData.remaining_usd -ne $null) {
    Write-Host "  Budget: `$$($statusData.spent_usd) spent / `$$($statusData.remaining_usd) remaining" -ForegroundColor Yellow
}
Write-Host ""

# ---- REPAY ----
Write-Host "Repaying credit line..." -ForegroundColor Cyan
$repayReq = @{
    jsonrpc = "2.0"; id = $idCounter; method = "tools/call"
    params = @{ name = "repay_credit"; arguments = @{ agent_id = $agentId } }
} | ConvertTo-Json -Depth 5 -Compress

$repayResp = Send-Mcp $repayReq
Flush-Stderr
Write-Host "  Credit line closed." -ForegroundColor Green

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host "  Simulation Complete!" -ForegroundColor Cyan
Write-Host "  $tradeCount trades executed across 3 rounds" -ForegroundColor White
Write-Host "  Dashboard still live at http://localhost:3030" -ForegroundColor White
Write-Host "=====================================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Press Enter to stop the server..." -ForegroundColor DarkGray
Read-Host | Out-Null

# Cleanup
$proc.Kill()
Get-EventSubscriber | Unregister-Event -Force 2>$null
Write-Host "[done] Server stopped." -ForegroundColor DarkGray
