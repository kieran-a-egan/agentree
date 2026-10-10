$ErrorActionPreference = 'Stop'
$bin = Join-Path $env:LOCALAPPDATA 'ClaudeAgentTree\bin'
# Only remove files in our dedicated Agentree installation directory.
$target = Join-Path $bin 'agentree.exe'
if (Test-Path -LiteralPath $target -PathType Leaf) { Remove-Item -LiteralPath $target -Force }
$userPath = [Environment]::GetEnvironmentVariable('Path','User')
if ($userPath) {
    $parts = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $bin.TrimEnd('\') })
    [Environment]::SetEnvironmentVariable('Path',($parts -join ';'),'User')
}
Write-Host 'Removed Agentree launcher. Claude Code has not been modified. Restart your terminal.' -ForegroundColor Green
