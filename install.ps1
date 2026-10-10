param([switch]$SkipBuild, [switch]$NoShim, [string]$ClaudeCli)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$homeDir = Join-Path $env:LOCALAPPDATA 'ClaudeAgentTree'
$bin = Join-Path $homeDir 'bin'
New-Item -ItemType Directory -Force -Path $bin | Out-Null

# Resolve the actual Claude Code CLI; never override the `claude` command.
if (-not $ClaudeCli) {
    $original = Get-Command claude -All -ErrorAction SilentlyContinue |
        Where-Object { $_.Source -and -not $_.Source.StartsWith($bin, [System.StringComparison]::OrdinalIgnoreCase) -and
                       ([IO.Path]::GetExtension($_.Source) -in @('.exe','.cmd','.bat')) } |
        Select-Object -First 1
    if ($original) { $ClaudeCli = $original.Source }
    else {
        $saved = Join-Path $bin 'claude-original.txt'
        if (Test-Path -LiteralPath $saved -PathType Leaf) {
            $candidate = (Get-Content -LiteralPath $saved -Raw).Trim()
            if (Test-Path -LiteralPath $candidate -PathType Leaf) { $ClaudeCli = $candidate }
        }
    }
    if (-not $ClaudeCli) { throw 'Claude Code CLI not found. Supply -ClaudeCli "C:\path\to\claude.exe".' }
}
$ClaudeCli = (Resolve-Path -LiteralPath $ClaudeCli).Path
if (-not (Test-Path -LiteralPath $ClaudeCli -PathType Leaf)) { throw "Claude Code executable not found: $ClaudeCli" }

if (-not $SkipBuild) {
    & cargo test
    if ($LASTEXITCODE -ne 0) { throw 'cargo test failed' }
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}
$exe = Join-Path $PSScriptRoot 'target\release\claude-agent-tree.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing compiled executable: $exe" }

# The actual command is a native EXE. Do not wrap it with cmd.exe, which causes
# "Terminate batch job (Y/N)?" when the command is interrupted.
$destination = Join-Path $bin 'agentree.exe'
Copy-Item -LiteralPath $exe -Destination $destination -Force
Set-Content -LiteralPath (Join-Path $bin 'claude-original.txt') -Value $ClaudeCli -Encoding UTF8

foreach ($shimName in @('agentree.cmd','claude.cmd')) {
    $legacyShim = Join-Path $bin $shimName
    if (Test-Path -LiteralPath $legacyShim -PathType Leaf) {
        $contents = Get-Content -LiteralPath $legacyShim -Raw
        $isOurs = $contents.Contains('claude-agent-tree.exe') -and
            ($shimName -eq 'agentree.cmd' -or $contents.Contains('CLAUDE_TREE_SHIM_DIR='))
        if ($isOurs) {
            Remove-Item -LiteralPath $legacyShim -Force
            Write-Host "Removed legacy Agentree batch wrapper: $shimName" -ForegroundColor Yellow
        } else {
            Write-Warning "Unrecognised $legacyShim left untouched. Check command precedence with Get-Command."
        }
    }
}

if (-not $NoShim) {
    $userPath = [Environment]::GetEnvironmentVariable('Path','User')
    $parts = @($userPath -split ';' | Where-Object { $_ -and $_.Trim() })
    $parts = @($parts | Where-Object { $_.TrimEnd('\') -ine $bin.TrimEnd('\') }) + @($bin)
    [Environment]::SetEnvironmentVariable('Path',($parts -join ';'),'User')
    Write-Host "Installed native command: $destination" -ForegroundColor Green
    Write-Host 'Restart Windows Terminal. Then run: agentree' -ForegroundColor Yellow
    Write-Host 'Normal Claude Code stays available as: claude' -ForegroundColor Green
    Write-Host "Embedded Claude backend: $ClaudeCli" -ForegroundColor Cyan
    if ([IO.Path]::GetExtension($ClaudeCli) -eq '.exe') {
        Write-Host 'If Claude still stalls inside ConPTY, try the npm Claude Code CLI:' -ForegroundColor Yellow
        Write-Host '  npm install -g @anthropic-ai/claude-code' -ForegroundColor Yellow
        Write-Host '  powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -SkipBuild -ClaudeCli "$env:APPDATA\npm\claude.cmd"' -ForegroundColor Yellow
    }
} else {
    Write-Host "Installed executable: $destination (PATH unchanged)" -ForegroundColor Green
}
