# Stops hydraui windows started from a cargo build (any target\debug or target\release,
# including worktrees'), leaving the installed hydraui alone: it hosts the terminals,
# Claude sessions among them, and every copy is called hydraui.exe, so stopping by name
# closes those too.
param([switch]$WhatIf)

$dev = @(Get-Process hydraui -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -match '\\target\\(debug|release)\\hydraui\.exe$' })
if ($dev.Count -eq 0) {
    Write-Host 'No dev hydraui running.'
    exit 0
}
foreach ($p in $dev) {
    Write-Host ("Stopping pid {0}  {1}" -f $p.Id, $p.Path)
    if (-not $WhatIf) { Stop-Process -Id $p.Id -Force }
}
