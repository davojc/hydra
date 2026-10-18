# Tests hydra-setup against the real system (spec §7.7): a silent install from local files into a
# temporary folder (twice, as an upgrade), then a silent uninstall, checking what each changes.
# Used by the release job before publishing; runnable locally. Restores the user PATH it found
# exactly (the raw registry value and its type, so %VARIABLES% stay unexpanded) and points
# HYDRA_HOME at a temporary folder, so ~/.hydra is never touched. It refuses to run where hydra
# is installed: the Settings entry and Start menu shortcut it checks are the real ones.
param(
    [Parameter(Mandatory)] [string]$Setup,
    [Parameter(Mandatory)] [string]$From,
    [Parameter(Mandatory)] [string]$Version
)
$ErrorActionPreference = 'Stop'
$failures = 0
function Check([bool]$ok, [string]$what) {
    if ($ok) { Write-Host "ok   $what" } else { Write-Host "FAIL $what" -ForegroundColor Red; $script:failures++ }
}
$envKey = 'HKCU:\Environment'
# The raw value: [Environment]::GetEnvironmentVariable expands %VARS%, and writing that back
# would turn the user's REG_EXPAND_SZ PATH into an expanded REG_SZ for good.
function UserPath { (Get-Item $envKey).GetValue('Path', $null, 'DoNotExpandEnvironmentNames') }
function SetUserPath($value, $kind) {
    if ($null -eq $value) { Remove-ItemProperty -Path $envKey -Name Path -ErrorAction SilentlyContinue }
    else { Set-ItemProperty -Path $envKey -Name Path -Value $value -Type $kind }
}
function Entries([string]$p) { if ($p) { @($p.Split(';') | Where-Object { $_ -ne '' }) } else { @() } }
function Same([string]$a, [string]$b) { $a.TrimEnd('\') -ieq $b.TrimEnd('\') }

$Setup = (Resolve-Path $Setup).Path
$From = (Resolve-Path $From).Path
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\hydraui.lnk'
if ((Test-Path $key) -or (Test-Path $lnk)) {
    Write-Host "hydra is installed on this machine (Settings entry or Start menu shortcut); this test would overwrite and then delete them. Run it on a machine without hydra installed, such as CI." -ForegroundColor Red
    exit 2
}
$saved = UserPath
$savedKind = if ($null -ne $saved) { (Get-Item $envKey).GetValueKind('Path') } else { 'ExpandString' }
$work = Join-Path ([IO.Path]::GetTempPath()) ("hydra-setup-test-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$dir = Join-Path $work 'app'
$neighbour = Join-Path $work 'Neighbour\'
$env:HYDRA_HOME = Join-Path $work 'home'
New-Item -ItemType Directory -Force $env:HYDRA_HOME | Out-Null
try {
    SetUserPath ((Entries $saved) + $neighbour -join ';') 'ExpandString'
    foreach ($round in 1, 2) {
        $p = Start-Process -FilePath $Setup -ArgumentList '--silent', '--from', "`"$From`"", '--dir', "`"$dir`"" -Wait -PassThru
        Check ($p.ExitCode -eq 0) "install $round exited 0 (got $($p.ExitCode))"
    }
    foreach ($f in 'hydraui.exe', 'hydra.exe', 'hydra-setup.exe') {
        Check (Test-Path (Join-Path $dir $f)) "$f installed"
    }
    $got = (& (Join-Path $dir 'hydra.exe') --version | Out-String).Trim()
    Check ($got -eq "hydra $Version") "hydra --version is 'hydra $Version' (got '$got')"
    $ours = @(Entries (UserPath) | Where-Object { Same $_ $dir })
    Check ($ours.Count -eq 1) "install folder on the user PATH once (found $($ours.Count))"
    $k = Get-ItemProperty $key -ErrorAction SilentlyContinue
    Check ($null -ne $k -and $k.DisplayVersion -eq $Version) "Settings entry has version $Version"
    Check ($null -ne $k -and (Same $k.InstallLocation $dir)) 'Settings entry points at the install folder'
    Check (Test-Path $lnk) 'Start menu shortcut made'
    Check (Test-Path (Join-Path $env:HYDRA_HOME 'config.toml')) 'hydra init made config.toml'

    $u = Start-Process -FilePath (Join-Path $dir 'hydra-setup.exe') -ArgumentList '--uninstall', '--silent' -Wait -PassThru
    Check ($u.ExitCode -eq 0) "uninstall exited 0 (got $($u.ExitCode))"
    Start-Sleep -Seconds 5   # the uninstaller deletes itself and the folder after it exits
    foreach ($f in 'hydraui.exe', 'hydra.exe', 'hydra-setup.exe') {
        Check (-not (Test-Path (Join-Path $dir $f))) "$f removed"
    }
    Check (@(Entries (UserPath) | Where-Object { Same $_ $dir }).Count -eq 0) 'install folder gone from the user PATH'
    Check (@(Entries (UserPath) | Where-Object { $_ -eq $neighbour }).Count -eq 1) 'other PATH entries untouched'
    Check (-not (Test-Path $key)) 'Settings entry removed'
    Check (-not (Test-Path $lnk)) 'Start menu shortcut removed'
    Check (Test-Path (Join-Path $env:HYDRA_HOME 'config.toml')) 'hydra home kept'
} finally {
    SetUserPath $saved $savedKind
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
if ($failures) { Write-Host "$failures check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host 'all checks passed'
