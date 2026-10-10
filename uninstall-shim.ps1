$ErrorActionPreference = 'Stop'
$bin = Join-Path $env:LOCALAPPDATA 'ClaudeAgentTree\bin'
# Only remove files in our dedicated Agentree installation directory.
$target = Join-Path $bin 'agentree.exe'
if (Test-Path -LiteralPath $target -PathType Leaf) { Remove-Item -LiteralPath $target -Force }
# Legacy batch wrappers from shim-era installs; only delete ones we recognise.
foreach ($file in @('agentree.cmd','claude.cmd')) {
    $legacy = Join-Path $bin $file
    if (Test-Path -LiteralPath $legacy -PathType Leaf) {
        $contents = Get-Content -LiteralPath $legacy -Raw
        $isOurs = $contents.Contains('claude-agent-tree.exe') -and
            ($file -eq 'agentree.cmd' -or $contents.Contains('CLAUDE_TREE_SHIM_DIR='))
        if ($isOurs) { Remove-Item -LiteralPath $legacy -Force; Write-Host "Removed legacy wrapper: $file" -ForegroundColor Yellow }
        else { Write-Warning "Unrecognised $file left untouched." }
    }
}
$userPath = [Environment]::GetEnvironmentVariable('Path','User')
if ($userPath) {
    $parts = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $bin.TrimEnd('\') })
    [Environment]::SetEnvironmentVariable('Path',($parts -join ';'),'User')
}
Write-Host 'Removed Agentree launcher. Claude Code has not been modified. Restart your terminal.' -ForegroundColor Green
