$ErrorActionPreference = 'Stop'
$bin = Join-Path $env:LOCALAPPDATA 'ClaudeAgentTree\bin'
# Only remove files in our dedicated Agentree installation directory.
foreach ($file in @('agentree.exe','agentree.cmd')) {
    $target = Join-Path $bin $file
    if (Test-Path -LiteralPath $target -PathType Leaf) {
        if ($file -eq 'agentree.exe') { Remove-Item -LiteralPath $target -Force }
        else {
            $contents = Get-Content -LiteralPath $target -Raw
            if ($contents.Contains('claude-agent-tree.exe')) { Remove-Item -LiteralPath $target -Force }
            else { Write-Warning "Unrecognised $file left untouched." }
        }
    }
}
$old = Join-Path $bin 'claude.cmd'
if (Test-Path -LiteralPath $old -PathType Leaf) {
    $contents = Get-Content -LiteralPath $old -Raw
    if ($contents.Contains('CLAUDE_TREE_SHIM_DIR=') -and $contents.Contains('claude-agent-tree.exe')) {
        Remove-Item -LiteralPath $old -Force
        Write-Host 'Removed legacy Claude command override.' -ForegroundColor Yellow
    } else { Write-Warning 'Unrecognised claude.cmd left untouched.' }
}
$userPath = [Environment]::GetEnvironmentVariable('Path','User')
if ($userPath) {
    $parts = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $bin.TrimEnd('\') })
    [Environment]::SetEnvironmentVariable('Path',($parts -join ';'),'User')
}
Write-Host 'Removed Agentree launcher. Claude Code has not been modified. Restart your terminal.' -ForegroundColor Green
