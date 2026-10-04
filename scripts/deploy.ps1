# Builds hydra and installs it to E:\Tools.
# Refuses to deploy while any hydra process is running, so only one version is ever installed.
param([string]$Target = 'E:\Tools')

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent

$running = @(Get-Process hydra, hydra.old -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    Write-Host "hydra is in use - not deploying. Close these first:" -ForegroundColor Yellow
    $running | ForEach-Object { Write-Host ("  pid {0}  started {1}  {2}" -f $_.Id, $_.StartTime, $_.Path) }
    exit 1
}

cargo build --release --manifest-path (Join-Path $repo 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Copy-Item (Join-Path $repo 'target\release\hydra.exe') (Join-Path $Target 'hydra.exe') -Force
Remove-Item (Join-Path $Target 'hydra.old.exe') -ErrorAction SilentlyContinue
& (Join-Path $Target 'hydra.exe') --version
