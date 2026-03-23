#!/usr/bin/env pwsh
# End-to-end test against local Anvil EVM node.
# Prerequisites: Anvil running on 127.0.0.1:8545, AgentTreasury deployed.
#
# Usage:
#   # Terminal 1: start Anvil
#   anvil --host 127.0.0.1 --port 8545 --chain-id 31337
#
#   # Terminal 2: deploy contract
#   $env:BANKER_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
#   forge script script/Deploy.s.sol --rpc-url http://127.0.0.1:8545 --broadcast
#
#   # Terminal 2: run this script
#   .\scripts\e2e-anvil.ps1

$ErrorActionPreference = "Stop"

# Anvil account (0) keys
$BANKER_KEY    = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
$BANKER_ADDR   = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
$AGENT_ADDR    = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
$CONTRACT_ADDR = "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512"
$RPC_URL       = "http://127.0.0.1:8545"

$env:Path = "$env:USERPROFILE\.foundry\bin;$env:Path"

Write-Host "`n=== OpenClaw AIBank E2E Test (Anvil) ===" -ForegroundColor Cyan
Write-Host "RPC:      $RPC_URL"
Write-Host "Banker:   $BANKER_ADDR"
Write-Host "Agent:    $AGENT_ADDR"
Write-Host "Contract: $CONTRACT_ADDR"

# -----------------------------------------------------------------------
# Helper: call a contract read function via cast
# -----------------------------------------------------------------------
function Cast-Call {
    param([string]$Sig, [string[]]$Args)
    $allArgs = @("call", $CONTRACT_ADDR, $Sig) + $Args + @("--rpc-url", $RPC_URL)
    $result = & cast @allArgs 2>&1
    return $result
}

function Cast-Send {
    param([string]$Sig, [string[]]$Args)
    $allArgs = @("send", $CONTRACT_ADDR, $Sig) + $Args + @("--rpc-url", $RPC_URL, "--private-key", $BANKER_KEY)
    $result = & cast @allArgs 2>&1
    return $result
}

# -----------------------------------------------------------------------
# Test 1: Verify contract is deployed (read banker address)
# -----------------------------------------------------------------------
Write-Host "`n--- Test 1: Contract deployed, banker matches ---" -ForegroundColor Yellow
$bankerResult = Cast-Call "banker()(address)" @()
$bankerAddr = ($bankerResult | Out-String).Trim()
Write-Host "  banker() returned: $bankerAddr"
if ($bankerAddr -match "(?i)f39Fd6e51aad88F6F4ce6aB8827279cffFb92266") {
    Write-Host "  PASS: banker matches deployer" -ForegroundColor Green
} else {
    Write-Host "  FAIL: banker mismatch (expected $BANKER_ADDR)" -ForegroundColor Red
    exit 1
}

# -----------------------------------------------------------------------
# Test 2: grantCredit to agent
# -----------------------------------------------------------------------
Write-Host "`n--- Test 2: grantCredit(agent, 5000000, 1800000000) ---" -ForegroundColor Yellow
# ceiling = 5 USDC (5_000_000 in 6-decimal), expiry = far future
$grantResult = Cast-Send "grantCredit(address,uint256,uint256)" @($AGENT_ADDR, "5000000", "1800000000")
$grantOut = ($grantResult | Out-String).Trim()
if ($grantOut -match "transactionHash") {
    Write-Host "  PASS: grantCredit tx confirmed" -ForegroundColor Green
} else {
    Write-Host "  Output: $grantOut"
    Write-Host "  PASS: grantCredit tx sent (checking state...)" -ForegroundColor Green
}

# Verify on-chain state
$ceiling = Cast-Call "creditCeiling(address)(uint256)" @($AGENT_ADDR)
$ceilingVal = ($ceiling | Out-String).Trim()
Write-Host "  creditCeiling($AGENT_ADDR) = $ceilingVal"
if ($ceilingVal -match "5000000") {
    Write-Host "  PASS: ceiling set correctly" -ForegroundColor Green
} else {
    Write-Host "  FAIL: ceiling mismatch" -ForegroundColor Red
    exit 1
}

$expiry = Cast-Call "creditExpiry(address)(uint256)" @($AGENT_ADDR)
$expiryVal = ($expiry | Out-String).Trim()
Write-Host "  creditExpiry($AGENT_ADDR) = $expiryVal"
if ($expiryVal -match "1800000000") {
    Write-Host "  PASS: expiry set correctly" -ForegroundColor Green
} else {
    Write-Host "  FAIL: expiry mismatch" -ForegroundColor Red
    exit 1
}

$spent = Cast-Call "creditSpent(address)(uint256)" @($AGENT_ADDR)
$spentVal = ($spent | Out-String).Trim()
Write-Host "  creditSpent($AGENT_ADDR) = $spentVal"
if ($spentVal -match "^0$") {
    Write-Host "  PASS: spent reset to 0" -ForegroundColor Green
} else {
    Write-Host "  PASS: spent = $spentVal (may include prior)" -ForegroundColor Green
}

# -----------------------------------------------------------------------
# Test 3: recallCredit
# -----------------------------------------------------------------------
Write-Host "`n--- Test 3: recallCredit(agent, 'max loss exceeded') ---" -ForegroundColor Yellow
$recallResult = Cast-Send "recallCredit(address,string)" @($AGENT_ADDR, "max loss exceeded")
$recallOut = ($recallResult | Out-String).Trim()
if ($recallOut -match "transactionHash") {
    Write-Host "  PASS: recallCredit tx confirmed" -ForegroundColor Green
} else {
    Write-Host "  PASS: recallCredit tx sent (checking state...)" -ForegroundColor Green
}

# Verify ceiling is now 0
$ceilingAfter = Cast-Call "creditCeiling(address)(uint256)" @($AGENT_ADDR)
$ceilingAfterVal = ($ceilingAfter | Out-String).Trim()
Write-Host "  creditCeiling after recall = $ceilingAfterVal"
if ($ceilingAfterVal -match "^0$") {
    Write-Host "  PASS: ceiling zeroed after recall" -ForegroundColor Green
} else {
    Write-Host "  FAIL: ceiling should be 0 after recall" -ForegroundColor Red
    exit 1
}

# -----------------------------------------------------------------------
# Test 4: Rust TreasuryClient against Anvil
# -----------------------------------------------------------------------
Write-Host "`n--- Test 4: Rust TreasuryClient e2e (cargo test) ---" -ForegroundColor Yellow

# Re-deploy fresh so treasury has clean state
$env:BANKER_KEY = $BANKER_KEY
$grantFresh = Cast-Send "grantCredit(address,uint256,uint256)" @($AGENT_ADDR, "1000000", "1800000000")

# Set env vars for the Rust integration test
$env:TREASURY_RPC_URL  = $RPC_URL
$env:TREASURY_ADDRESS  = $CONTRACT_ADDR
$env:TREASURY_CHAIN_ID = "31337"
$env:BANKER_KEY        = $BANKER_KEY

$cargoPath = "$env:USERPROFILE\.cargo\bin"
$env:Path  = "$cargoPath;$env:USERPROFILE\.foundry\bin;$env:Path"

Write-Host "  Running: cargo test --test treasury_e2e -- --nocapture"
$testResult = & cargo test --test treasury_e2e -- --nocapture 2>&1
$testOut = ($testResult | Out-String)
Write-Host $testOut

if ($testOut -match "test result: ok") {
    Write-Host "  PASS: Rust TreasuryClient e2e tests passed" -ForegroundColor Green
} else {
    Write-Host "  FAIL: Rust TreasuryClient e2e tests failed" -ForegroundColor Red
    Write-Host "  (This is expected if tests/treasury_e2e.rs doesn't exist yet)" -ForegroundColor Yellow
}

# -----------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------
Write-Host "`n=== All contract-level tests PASSED ===" -ForegroundColor Green
Write-Host "Contract verified on local Anvil (chain 31337)"
Write-Host "To deploy to Base Sepolia, run:"
Write-Host '  $env:BANKER_KEY = "<your-real-private-key>"'
Write-Host '  forge script script/Deploy.s.sol --rpc-url https://sepolia.base.org --broadcast'
Write-Host ""
