param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Rust Cargo is missing: install https://rustup.rs' }
if (-not $SkipTests) {
    & cargo test
    if ($LASTEXITCODE -ne 0) { throw 'cargo test failed' }
}
& cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
Write-Host "Compiled: $PSScriptRoot\target\release\claude-agent-tree.exe" -ForegroundColor Green
