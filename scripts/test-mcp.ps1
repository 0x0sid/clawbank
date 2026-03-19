# Test the MCP skill locally by sending JSON-RPC requests via stdin.
# Usage: cat scripts\test-mcp.ps1 (read this), then run the binary and pipe requests.
#
# Quick test:
#   echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | .\target\release\openclaw-aibank.exe

$binary = ".\target\release\openclaw-aibank.exe"

Write-Host "=== OpenClaw AI Bank — MCP Local Test ===" -ForegroundColor Cyan
Write-Host ""

# Step 1: List tools
Write-Host "[1] Listing MCP tools..." -ForegroundColor Yellow
$listTools = '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
Write-Host "  Request: $listTools"
$result = echo $listTools | & $binary 2>$null
Write-Host "  Response: $result" -ForegroundColor Green
Write-Host ""

# Step 2: Register an agent
Write-Host "[2] Registering test agent..." -ForegroundColor Yellow
$register = '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agent_register","arguments":{"name":"test-agent-01"}}}'
Write-Host "  Request: $register"
$result = echo $register | & $binary 2>$null
Write-Host "  Response: $result" -ForegroundColor Green
Write-Host ""

Write-Host "=== Test complete ===" -ForegroundColor Cyan
Write-Host ""
Write-Host "Dashboard should be running at http://localhost:3030" -ForegroundColor Cyan
Write-Host "Set RUST_LOG=debug for verbose output on stderr" -ForegroundColor Gray
