##############################################################################
# macd-bot.ps1 — MACD + Volume Trend trading bot for OpenClaw
#
# Fetches real BTC-USDT candles from OKX public API (no auth needed),
# computes MACD(12,26,9) + volume trend, simulates $10 of trading,
# reports everything to the OpenClaw dashboard in real-time.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File .\scripts\macd-bot.ps1
#
# The openclaw-aibank dashboard must be running on http://localhost:3030
##############################################################################

$ErrorActionPreference = "Stop"
$BASE = "http://localhost:3030"
$OKX_PUBLIC = "https://www.okx.com"
$PAIR = "BTC-USDT"
$CAPITAL = 10.0          # Simulated starting capital in USD
$TRADE_SIZE = 1.0        # USD per trade (respects $1 hard cap)
$BACKTEST_CANDLES = 60   # 1h of 1-minute candles for backtest
$LIVE_MINUTES = 5        # Live monitoring duration after backtest

# ---------- Helpers ----------

function Log($color, $msg) {
    $ts = Get-Date -Format "HH:mm:ss"
    Write-Host "[$ts] " -NoNewline -ForegroundColor DarkGray
    Write-Host $msg -ForegroundColor $color
}

function Api($method, $path, $body) {
    $params = @{ Uri = "$BASE$path"; Method = $method; ContentType = "application/json" }
    if ($body) { $params.Body = ($body | ConvertTo-Json -Compress) }
    try { Invoke-RestMethod @params } catch {
        Log Red "API error $path : $_"
        $null
    }
}

function BotReport($agentId, $msg) {
    Api "POST" "/api/bot/report" @{ agent_id = $agentId; message = $msg } | Out-Null
    Log Cyan ">> $msg"
}

# ---------- OKX Public API: Fetch Candles ----------

function Get-Candles($instId, $bar, $limit) {
    $url = "$OKX_PUBLIC/api/v5/market/candles?instId=$instId&bar=$bar&limit=$limit"
    $resp = Invoke-RestMethod -Uri $url -Method GET
    if ($resp.code -ne "0") {
        Log Red "OKX candle fetch failed: $($resp.msg)"
        return @()
    }
    # OKX returns: [ts, open, high, low, close, vol, volCcy, volCcyQuote, confirm]
    # Sorted newest first — reverse for chronological order
    $candles = @()
    for ($i = $resp.data.Count - 1; $i -ge 0; $i--) {
        $c = $resp.data[$i]
        $candles += [PSCustomObject]@{
            ts     = [long]$c[0]
            open   = [double]$c[1]
            high   = [double]$c[2]
            low    = [double]$c[3]
            close  = [double]$c[4]
            vol    = [double]$c[5]
        }
    }
    return $candles
}

# ---------- MACD Calculation ----------

function Calc-EMA($data, $period) {
    $multiplier = 2.0 / ($period + 1)
    $ema = @($data[0])
    for ($i = 1; $i -lt $data.Count; $i++) {
        $ema += ($data[$i] - $ema[$i - 1]) * $multiplier + $ema[$i - 1]
    }
    return $ema
}

function Calc-MACD($closes) {
    $ema12 = Calc-EMA $closes 12
    $ema26 = Calc-EMA $closes 26
    $macdLine = @()
    for ($i = 0; $i -lt $closes.Count; $i++) {
        $macdLine += ($ema12[$i] - $ema26[$i])
    }
    $signal = Calc-EMA $macdLine 9
    $histogram = @()
    for ($i = 0; $i -lt $closes.Count; $i++) {
        $histogram += ($macdLine[$i] - $signal[$i])
    }
    return @{ macd = $macdLine; signal = $signal; histogram = $histogram }
}

# ---------- Volume Trend ----------

function Calc-VolumeTrend($volumes, $period) {
    # Simple: compare recent avg volume to longer avg
    $result = @()
    for ($i = 0; $i -lt $volumes.Count; $i++) {
        if ($i -lt $period) {
            $result += 0.0
        } else {
            $recent = ($volumes[($i - [Math]::Floor($period/2))..($i)] | Measure-Object -Average).Average
            $older  = ($volumes[($i - $period)..($i - [Math]::Floor($period/2) - 1)] | Measure-Object -Average).Average
            if ($older -gt 0) {
                $result += ($recent / $older) - 1.0
            } else {
                $result += 0.0
            }
        }
    }
    return $result
}

# ---------- Strategy: MACD + Volume Trend ----------

function Run-Strategy($candles) {
    $closes  = $candles | ForEach-Object { $_.close }
    $volumes = $candles | ForEach-Object { $_.vol }

    $macd = Calc-MACD $closes
    $volTrend = Calc-VolumeTrend $volumes 10

    $trades = @()
    $position = $null   # null = no position, otherwise { entry_price, size_usd, entry_idx }
    $balance = $CAPITAL
    $totalPnl = 0.0

    # Start from candle 26 (need enough data for MACD)
    for ($i = 27; $i -lt $candles.Count; $i++) {
        $hist = $macd.histogram[$i]
        $prevHist = $macd.histogram[$i - 1]
        $vt = $volTrend[$i]
        $price = $candles[$i].close

        # BUY signal: histogram crosses above zero + volume increasing
        if ($null -eq $position -and $prevHist -le 0 -and $hist -gt 0 -and $vt -gt 0.05) {
            $size = [Math]::Min($TRADE_SIZE, $balance)
            if ($size -gt 0.10) {
                $position = @{ entry_price = $price; size_usd = $size; entry_idx = $i }
                $balance -= $size
                $trades += [PSCustomObject]@{
                    idx    = $i
                    time   = [DateTimeOffset]::FromUnixTimeMilliseconds($candles[$i].ts).DateTime.ToString("HH:mm")
                    side   = "BUY"
                    price  = $price
                    size   = $size
                    pnl    = 0.0
                    reason = "MACD cross up + vol trend +$([Math]::Round($vt*100,1))%"
                }
            }
        }

        # SELL signal: histogram crosses below zero OR stop-loss at -2%
        if ($null -ne $position) {
            $pctChange = ($price - $position.entry_price) / $position.entry_price
            $shouldSell = ($prevHist -ge 0 -and $hist -lt 0) -or ($pctChange -le -0.02)

            if ($shouldSell) {
                $pnl = $position.size_usd * $pctChange
                $balance += $position.size_usd + $pnl
                $totalPnl += $pnl
                $reason = if ($pctChange -le -0.02) { "Stop-loss -2%" } else { "MACD cross down" }
                $trades += [PSCustomObject]@{
                    idx    = $i
                    time   = [DateTimeOffset]::FromUnixTimeMilliseconds($candles[$i].ts).DateTime.ToString("HH:mm")
                    side   = "SELL"
                    price  = $price
                    size   = $position.size_usd
                    pnl    = [Math]::Round($pnl, 4)
                    reason = "$reason (pnl: $([Math]::Round($pnl, 4)))"
                }
                $position = $null
            }
        }
    }

    # Close any open position at last price
    if ($null -ne $position) {
        $lastPrice = $candles[-1].close
        $pctChange = ($lastPrice - $position.entry_price) / $position.entry_price
        $pnl = $position.size_usd * $pctChange
        $balance += $position.size_usd + $pnl
        $totalPnl += $pnl
        $trades += [PSCustomObject]@{
            idx    = $candles.Count - 1
            time   = [DateTimeOffset]::FromUnixTimeMilliseconds($candles[-1].ts).DateTime.ToString("HH:mm")
            side   = "CLOSE"
            price  = $lastPrice
            size   = $position.size_usd
            pnl    = [Math]::Round($pnl, 4)
            reason = "End of window (pnl: $([Math]::Round($pnl, 4)))"
        }
    }

    return @{
        trades    = $trades
        total_pnl = [Math]::Round($totalPnl, 4)
        balance   = [Math]::Round($balance, 4)
        return_pct = [Math]::Round(($totalPnl / $CAPITAL) * 100, 2)
        num_trades = $trades.Count
    }
}

##############################################################################
# MAIN
##############################################################################

Write-Host ""
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  OpenClaw MACD Bot — BTC-USDT Short-Term Strategy" -ForegroundColor Cyan
Write-Host "  Capital: `$$CAPITAL | Pair: $PAIR | Candles: 1m" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host ""

# --- Step 1: Register bot agent ---
Log Yellow "[1/5] Registering bot agent..."
$reg = Api "POST" "/api/bot/register" @{ name = "MACD-Bot" }
if (-not $reg -or -not $reg.ok) {
    Log Red "Failed to register bot. Is the dashboard running on $BASE?"
    exit 1
}
$AGENT_ID = $reg.agent_id
Log Green "Registered: $($reg.name) (ID: $($AGENT_ID.Substring(0,8))...)"

# --- Step 2: Fetch real candles ---
Log Yellow "[2/5] Fetching $BACKTEST_CANDLES x 1m candles for $PAIR from OKX..."
$candles = Get-Candles $PAIR "1m" $BACKTEST_CANDLES
if ($candles.Count -lt 30) {
    Log Red "Not enough candles ($($candles.Count)). Need at least 30."
    exit 1
}
$firstPrice = $candles[0].close
$lastPrice = $candles[-1].close
Log Green "Got $($candles.Count) candles | Range: $($candles[0].open) -> $lastPrice | Span: $([Math]::Round(($candles[-1].ts - $candles[0].ts) / 60000, 0))min"

BotReport $AGENT_ID "Starting backtest: $($candles.Count) candles, capital `$$CAPITAL, pair $PAIR"

# --- Step 3: Run MACD strategy backtest ---
Log Yellow "[3/5] Running MACD(12,26,9) + Volume Trend strategy..."
$result = Run-Strategy $candles

Write-Host ""
Write-Host "--- Backtest Results ---" -ForegroundColor Yellow
Write-Host "  Trades executed: $($result.num_trades)" -ForegroundColor White
Write-Host "  Total PnL:       " -NoNewline
if ($result.total_pnl -ge 0) {
    Write-Host "+`$$($result.total_pnl) ($($result.return_pct)%)" -ForegroundColor Green
} else {
    Write-Host "-`$$([Math]::Abs($result.total_pnl)) ($($result.return_pct)%)" -ForegroundColor Red
}
Write-Host "  Final balance:   `$$($result.balance)" -ForegroundColor White
Write-Host ""

if ($result.trades.Count -gt 0) {
    Write-Host "  Trade Log:" -ForegroundColor Yellow
    foreach ($t in $result.trades) {
        $color = if ($t.side -eq "BUY") { "Green" } elseif ($t.pnl -ge 0) { "Green" } else { "Red" }
        Write-Host "    $($t.time) $($t.side.PadRight(5)) @ `$$([Math]::Round($t.price,2).ToString().PadRight(10)) `$$($t.size) | $($t.reason)" -ForegroundColor $color
    }
    Write-Host ""
}

# Report backtest results to dashboard
$pnlStr = if ($result.total_pnl -ge 0) { "+$($result.total_pnl)" } else { "$($result.total_pnl)" }
BotReport $AGENT_ID "Backtest complete: $($result.num_trades) trades, PnL $pnlStr ($($result.return_pct)%), balance `$$($result.balance)"

# --- Step 4: Decision — request budget or improve? ---
Log Yellow "[4/5] Evaluating results..."

if ($result.return_pct -ge 0) {
    Log Green "POSITIVE return ($($result.return_pct)%) — requesting `$$CAPITAL credit to go live!"
    BotReport $AGENT_ID "Strategy profitable ($($result.return_pct)%). Requesting `$$CAPITAL budget for live trading."

    $credit = Api "POST" "/api/bot/request-credit" @{
        agent_id       = $AGENT_ID
        amount_usd     = $CAPITAL
        strategy       = "MACD(12,26,9) + Volume Trend on $PAIR 1m candles"
        duration_hours = 24
    }

    if ($credit -and $credit.ok) {
        Log Green "Credit proposal submitted (ID: $($credit.proposal_id.Substring(0,8))...)"
        Log Yellow ">>> Go to http://localhost:3030 and APPROVE the proposal to activate live trading <<<"
        BotReport $AGENT_ID "Credit proposal submitted. Awaiting human approval on dashboard."
    } else {
        Log Red "Credit request failed: $($credit.error)"
    }
} else {
    Log Red "NEGATIVE return ($($result.return_pct)%) — strategy needs improvement."
    BotReport $AGENT_ID "Strategy unprofitable ($($result.return_pct)%). Analyzing improvements..."

    Write-Host ""
    Write-Host "--- Strategy Improvement Suggestions ---" -ForegroundColor Yellow
    Write-Host "  1. Tighten MACD parameters (try 8,21,5 for faster signals)" -ForegroundColor White
    Write-Host "  2. Add RSI filter (only buy when RSI < 40)" -ForegroundColor White
    Write-Host "  3. Increase volume threshold (require +10% instead of +5%)" -ForegroundColor White
    Write-Host "  4. Try 5m candles instead of 1m for less noise" -ForegroundColor White
    Write-Host "  5. Add support/resistance levels as confirmation" -ForegroundColor White
    Write-Host ""

    BotReport $AGENT_ID "Suggested improvements: tighter MACD params, RSI filter, higher vol threshold, 5m candles"
}

# --- Step 5: Live monitoring loop ---
Log Yellow "[5/5] Starting live monitoring for $LIVE_MINUTES minutes..."
BotReport $AGENT_ID "Entering live monitoring mode ($LIVE_MINUTES min)..."

$liveStart = Get-Date
$liveTrades = 0
$livePnl = 0.0
$livePosition = $null

for ($tick = 0; $tick -lt $LIVE_MINUTES; $tick++) {
    Start-Sleep -Seconds 60

    # Fetch fresh candles
    $freshCandles = Get-Candles $PAIR "1m" 30
    if ($freshCandles.Count -lt 28) { continue }

    $closes = $freshCandles | ForEach-Object { $_.close }
    $volumes = $freshCandles | ForEach-Object { $_.vol }
    $macd = Calc-MACD $closes
    $vt = (Calc-VolumeTrend $volumes 10)[-1]
    $lastIdx = $freshCandles.Count - 1
    $price = $freshCandles[$lastIdx].close
    $hist = $macd.histogram[$lastIdx]
    $prevHist = $macd.histogram[$lastIdx - 1]

    $elapsed = [Math]::Round(((Get-Date) - $liveStart).TotalMinutes, 1)
    Log DarkGray "  tick $($tick+1)/$LIVE_MINUTES | BTC=$([Math]::Round($price,2)) | MACD hist=$([Math]::Round($hist,2)) | vol=$([Math]::Round($vt*100,1))%"

    # Buy signal
    if ($null -eq $livePosition -and $prevHist -le 0 -and $hist -gt 0 -and $vt -gt 0.05) {
        $livePosition = @{ entry_price = $price; size_usd = $TRADE_SIZE }
        $liveTrades++
        Log Green "  LIVE BUY @ $([Math]::Round($price,2)) ($TRADE_SIZE USD) | MACD cross + vol"
        BotReport $AGENT_ID "[LIVE] BUY $PAIR @ `$$([Math]::Round($price,2)) | MACD cross up + vol +$([Math]::Round($vt*100,1))%"
    }

    # Sell signal
    if ($null -ne $livePosition) {
        $pctChange = ($price - $livePosition.entry_price) / $livePosition.entry_price
        if (($prevHist -ge 0 -and $hist -lt 0) -or ($pctChange -le -0.02)) {
            $pnl = [Math]::Round($livePosition.size_usd * $pctChange, 4)
            $livePnl += $pnl
            $liveTrades++
            $reason = if ($pctChange -le -0.02) { "stop-loss" } else { "MACD cross down" }
            Log $(if ($pnl -ge 0) { "Green" } else { "Red" }) "  LIVE SELL @ $([Math]::Round($price,2)) | pnl=$pnl | $reason"
            BotReport $AGENT_ID "[LIVE] SELL $PAIR @ `$$([Math]::Round($price,2)) | PnL `$$pnl | $reason"
            $livePosition = $null
        }
    }
}

# Close any remaining live position
if ($null -ne $livePosition) {
    $freshCandles = Get-Candles $PAIR "1m" 1
    $finalPrice = $freshCandles[-1].close
    $pctChange = ($finalPrice - $livePosition.entry_price) / $livePosition.entry_price
    $pnl = [Math]::Round($livePosition.size_usd * $pctChange, 4)
    $livePnl += $pnl
    Log Yellow "  Closing live position @ $([Math]::Round($finalPrice,2)) | pnl=$pnl"
    BotReport $AGENT_ID "[LIVE] Closed remaining position @ `$$([Math]::Round($finalPrice,2)) | PnL `$$pnl"
}

$livePnl = [Math]::Round($livePnl, 4)

# --- Final Report ---
Write-Host ""
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  FINAL REPORT" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  Backtest:  $($result.num_trades) trades | PnL `$$($result.total_pnl) ($($result.return_pct)%)" -ForegroundColor White
Write-Host "  Live:      $liveTrades signals  | PnL `$$livePnl" -ForegroundColor White
$totalPnl = [Math]::Round($result.total_pnl + $livePnl, 4)
$totalRet = [Math]::Round(($totalPnl / $CAPITAL) * 100, 2)
Write-Host "  Combined:  PnL " -NoNewline -ForegroundColor White
if ($totalPnl -ge 0) {
    Write-Host "+`$$totalPnl ($totalRet%)" -ForegroundColor Green
} else {
    Write-Host "-`$$([Math]::Abs($totalPnl)) ($totalRet%)" -ForegroundColor Red
}
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host ""

BotReport $AGENT_ID "FINAL: backtest PnL `$$($result.total_pnl), live PnL `$$livePnl, total `$$totalPnl ($totalRet%)"

if ($totalPnl -ge 0) {
    Log Green "Strategy is profitable. Budget proposal is pending on the dashboard."
    Log Green "Open http://localhost:3030 to approve and start live trading."
} else {
    Log Yellow "Strategy needs tuning. Review the suggestions above and re-run."
}

Write-Host ""
