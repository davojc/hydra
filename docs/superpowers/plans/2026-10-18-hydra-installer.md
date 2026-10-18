# hydra Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each release ships `hydra-setup-x86_64.exe`, a per-user installer for hydra and hydraui, tested in CI; the website and README offer it first, with the portable exes beside it.

**Architecture:** An Inno Setup script (`installer/hydra.iss`) packages the two exes the release job already builds. A PowerShell test script (`installer/test-installer.ps1`) installs silently into a temporary folder, checks the result, uninstalls and checks again; CI runs it before publishing, and it can run locally. hydraui gains a small step after an in-app update that refreshes the version Settings → Apps shows. The site and README get an installer-first download section.

**Tech Stack:** Inno Setup 6 (preinstalled on GitHub's `windows-latest` runners; `ISCC.exe`), Pascal script in the `.iss` `[Code]` section, PowerShell 7, Rust (hydraui), HTML/CSS (site).

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md` §7.7 (Installer).

## Global Constraints

- Installer file name: `hydra-setup-x86_64.exe`, with `hydra-setup-x86_64.exe.sha256` (`<sha256>  hydra-setup-x86_64.exe`, lowercase hex).
- Per user, no admin prompt: `PrivilegesRequired=lowest`; install folder `%LOCALAPPDATA%\Programs\hydra` holding `hydraui.exe` and `hydra.exe`.
- Fixed `AppId` `{8C3F6B1E-5A2D-4E7B-9F10-3D2C6A4B7E91}` (never change it: upgrades and uninstall depend on it). Uninstall key: `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C3F6B1E-5A2D-4E7B-9F10-3D2C6A4B7E91}_is1`.
- User `PATH` (`HKCU\Environment`, `REG_EXPAND_SZ`) gets the folder once (case-insensitive, trailing `\` ignored); uninstall removes exactly that entry.
- Another `hydra.exe` on `PATH`: a message naming its folder, nothing changed; nothing shown when silent.
- `hydra init` runs hidden only when `<HYDRA_HOME or %USERPROFILE%\.hydra>\config.toml` is missing.
- Uninstall removes both exes, `*.old.exe`, `*.new.exe`, shortcuts and the `PATH` entry; `~/.hydra` stays.
- Portable exes stay in every release (the updaters download them).
- Tests never touch the real `~/.hydra`, Credential Manager or global git config. The installer test runs with `HYDRA_HOME` pointed at a temporary folder and restores the user `PATH` it found.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01EspxdxYwY5hiZXGTSESW37`; never `--no-verify`. Gate before each commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (its own exit status), `cargo test --workspace`.

## Review Focus

1. Installing twice (an upgrade) must not add the folder to `PATH` twice. Pinned in Task 1 (`test-installer.ps1` installs twice and counts entries).
2. Uninstall must leave every other `PATH` entry untouched, including ones that differ from ours only by case or a trailing `\`. Pinned in Task 1 (the test plants a neighbour entry and checks it survives).
3. An existing `~/.hydra/config.toml` must not be overwritten by `hydra init`. Pinned in Task 1 (the test plants a config file and checks it is unchanged).
4. A user `PATH` that is empty or missing must work (installer adds the first entry; uninstall leaves it empty, not a stray `;`). Pinned in Task 1 (the test runs once with `Path` deleted).
5. hydraui must not write `DisplayVersion` when it isn't running from the installed folder (a portable copy updating itself). Pinned in Task 2 (`only_the_installed_copy_records_its_version`).

---

### Task 1: The installer and its test

**Files:**
- Create: `installer/hydra.iss`, `installer/test-installer.ps1`
- Test: `installer/test-installer.ps1` (run locally against a locally built installer)

**Interfaces:**
- Produces: `ISCC.exe /DAppVersion=<x.y.z> /DDistDir=<dir with the two release exes> installer\hydra.iss` writes `<DistDir>\hydra-setup-x86_64.exe`. `pwsh installer\test-installer.ps1 -Setup <path> -Version <x.y.z>` exits 0 when every check passes, non-zero with the failed check otherwise.

- [ ] **Step 1: Write the test script** (`installer/test-installer.ps1`)

```powershell
# Installs hydra-setup silently into a temporary folder, checks the result, uninstalls and checks
# again. Used by the release job; runnable locally. Restores the user PATH it found.
param(
    [Parameter(Mandatory)] [string]$Setup,
    [Parameter(Mandatory)] [string]$Version
)
$ErrorActionPreference = 'Stop'
$failures = 0
function Check([bool]$ok, [string]$what) {
    if ($ok) { Write-Host "ok   $what" } else { Write-Host "FAIL $what" -ForegroundColor Red; $script:failures++ }
}
function UserPath { [Environment]::GetEnvironmentVariable('Path', 'User') }
function Entries([string]$p) { if ($p) { $p.Split(';') | Where-Object { $_ -ne '' } } else { @() } }
function Same([string]$a, [string]$b) { $a.TrimEnd('\') -ieq $b.TrimEnd('\') }

$saved = UserPath
$work = Join-Path ([IO.Path]::GetTempPath()) ("hydra-installer-test-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$dir = Join-Path $work 'app'
$home2 = Join-Path $work 'home'
New-Item -ItemType Directory -Force $home2 | Out-Null
$env:HYDRA_HOME = $home2
$neighbour = Join-Path $work 'Neighbour'
try {
    foreach ($round in 'with a PATH', 'with no PATH') {
        Write-Host "--- $round"
        if ($round -eq 'with a PATH') {
            # A neighbour that differs from nothing of ours, and our folder's case-twin must not exist yet.
            [Environment]::SetEnvironmentVariable('Path', ((Entries $saved) + "$neighbour\" -join ';'), 'User')
            Set-Content -Path (Join-Path $home2 'config.toml') -Value '# planted' -NoNewline
        } else {
            [Environment]::SetEnvironmentVariable('Path', $null, 'User')
            Remove-Item (Join-Path $home2 'config.toml') -ErrorAction SilentlyContinue
        }
        $args1 = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=$dir")
        Start-Process -FilePath $Setup -ArgumentList $args1 -Wait
        Start-Process -FilePath $Setup -ArgumentList $args1 -Wait   # an upgrade over itself
        Check (Test-Path (Join-Path $dir 'hydraui.exe')) 'hydraui.exe installed'
        Check (Test-Path (Join-Path $dir 'hydra.exe')) 'hydra.exe installed'
        $got = (& (Join-Path $dir 'hydra.exe') --version).Trim()
        Check ($got -eq "hydra $Version") "hydra --version is 'hydra $Version' (got '$got')"
        $ours = @(Entries (UserPath) | Where-Object { Same $_ $dir })
        Check ($ours.Count -eq 1) "install folder on the user PATH once (found $($ours.Count))"
        if ($round -eq 'with a PATH') {
            Check ((Get-Content (Join-Path $home2 'config.toml') -Raw) -eq '# planted') 'existing config.toml left alone'
        } else {
            Check (Test-Path (Join-Path $home2 'config.toml')) 'hydra init made config.toml'
        }
        Start-Process -FilePath (Join-Path $dir 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait
        Start-Sleep -Seconds 2   # the uninstaller deletes its own folder after it exits
        Check (-not (Test-Path (Join-Path $dir 'hydra.exe'))) 'hydra.exe removed'
        Check (-not (Test-Path (Join-Path $dir 'hydraui.exe'))) 'hydraui.exe removed'
        $left = @(Entries (UserPath) | Where-Object { Same $_ $dir })
        Check ($left.Count -eq 0) 'install folder gone from the user PATH'
        if ($round -eq 'with a PATH') {
            Check (@(Entries (UserPath) | Where-Object { $_ -eq "$neighbour\" }).Count -eq 1) 'other PATH entries untouched'
        } else {
            $p = UserPath
            Check ([string]::IsNullOrEmpty($p)) "user PATH left empty (got '$p')"
        }
    }
} finally {
    [Environment]::SetEnvironmentVariable('Path', $saved, 'User')
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
if ($failures) { Write-Host "$failures check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host 'all checks passed'
```

- [ ] **Step 2: Run it to see it fail**

Run: `pwsh installer/test-installer.ps1 -Setup dist/hydra-setup-x86_64.exe -Version 0.6.9`
Expected: fails at the first `Start-Process` (the installer doesn't exist yet).

- [ ] **Step 3: Write the installer** (`installer/hydra.iss`)

```iss
; hydra's installer (spec §7.7): hydraui and hydra for the current user, no admin prompt.
; Build: ISCC.exe /DAppVersion=0.6.3 /DDistDir=<folder with the two release exes> installer\hydra.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef DistDir
  #define DistDir "..\dist"
#endif

[Setup]
; Never change AppId: upgrades and uninstall find the installation by it.
AppId={{8C3F6B1E-5A2D-4E7B-9F10-3D2C6A4B7E91}
AppName=hydra
AppVersion={#AppVersion}
AppVerName=hydra {#AppVersion}
AppPublisher=davojc
AppPublisherURL=https://github.com/davojc/hydra
AppSupportURL=https://github.com/davojc/hydra/issues
DefaultDirName={localappdata}\Programs\hydra
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#DistDir}
OutputBaseFilename=hydra-setup-x86_64
SetupIconFile=..\assets\hydra.ico
UninstallDisplayIcon={app}\hydraui.exe
UninstallDisplayName=hydra
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut for hydraui"; Flags: unchecked

[Files]
Source: "{#DistDir}\hydraui-x86_64-pc-windows-msvc.exe"; DestDir: "{app}"; DestName: "hydraui.exe"; Flags: ignoreversion
Source: "{#DistDir}\hydra-x86_64-pc-windows-msvc.exe"; DestDir: "{app}"; DestName: "hydra.exe"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\hydraui"; Filename: "{app}\hydraui.exe"
Name: "{userdesktop}\hydraui"; Filename: "{app}\hydraui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\hydra.exe"; Parameters: "init"; Flags: runhidden waituntilterminated; Check: NeedsInit
Filename: "{app}\hydraui.exe"; Description: "Launch hydraui"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\*.old.exe"
Type: files; Name: "{app}\*.new.exe"

[Code]
const
  EnvKey = 'Environment';

function NoSlash(const S: String): String;
begin
  Result := S;
  while (Length(Result) > 0) and (Result[Length(Result)] = '\') do
    Delete(Result, Length(Result), 1);
end;

function SameDir(const A, B: String): Boolean;
begin
  Result := CompareText(NoSlash(A), NoSlash(B)) = 0;
end;

{ The user PATH's entries, without empty ones. }
function PathEntries(const Path: String): TArrayOfString;
var
  Rest, Part: String;
  P, N: Integer;
begin
  SetArrayLength(Result, 0);
  Rest := Path;
  while Rest <> '' do
  begin
    P := Pos(';', Rest);
    if P = 0 then begin Part := Rest; Rest := ''; end
    else begin Part := Copy(Rest, 1, P - 1); Delete(Rest, 1, P); end;
    if Part <> '' then
    begin
      N := GetArrayLength(Result);
      SetArrayLength(Result, N + 1);
      Result[N] := Part;
    end;
  end;
end;

function JoinEntries(const E: TArrayOfString): String;
var
  I: Integer;
begin
  Result := '';
  for I := 0 to GetArrayLength(E) - 1 do
  begin
    if Result <> '' then Result := Result + ';';
    Result := Result + E[I];
  end;
end;

function UserPath(): String;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Result) then
    Result := '';
end;

procedure SetUserPath(const Value: String);
begin
  if Value = '' then
    RegDeleteValue(HKEY_CURRENT_USER, EnvKey, 'Path')
  else
    RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Value);
end;

procedure AddToPath(const Dir: String);
var
  E: TArrayOfString;
  I, N: Integer;
begin
  E := PathEntries(UserPath());
  for I := 0 to GetArrayLength(E) - 1 do
    if SameDir(E[I], Dir) then Exit;
  N := GetArrayLength(E);
  SetArrayLength(E, N + 1);
  E[N] := Dir;
  SetUserPath(JoinEntries(E));
end;

procedure RemoveFromPath(const Dir: String);
var
  E, Kept: TArrayOfString;
  I, N: Integer;
begin
  E := PathEntries(UserPath());
  SetArrayLength(Kept, 0);
  for I := 0 to GetArrayLength(E) - 1 do
    if not SameDir(E[I], Dir) then
    begin
      N := GetArrayLength(Kept);
      SetArrayLength(Kept, N + 1);
      Kept[N] := E[I];
    end;
  SetUserPath(JoinEntries(Kept));
end;

function HydraHome(): String;
begin
  Result := GetEnv('HYDRA_HOME');
  if Result = '' then
    Result := ExpandConstant('{%USERPROFILE}') + '\.hydra';
end;

function NeedsInit(): Boolean;
begin
  Result := not FileExists(HydraHome() + '\config.toml');
end;

{ Another hydra.exe on PATH (this process's, which predates our entry): name its folder. }
procedure WarnOtherHydra();
var
  E: TArrayOfString;
  I: Integer;
begin
  E := PathEntries(GetEnv('PATH'));
  for I := 0 to GetArrayLength(E) - 1 do
    if (not SameDir(E[I], ExpandConstant('{app}'))) and FileExists(NoSlash(E[I]) + '\hydra.exe') then
    begin
      Log('another hydra.exe on PATH: ' + E[I]);
      if not WizardSilent() then
        MsgBox('Another hydra.exe is on your PATH, in ' + E[I] + '.' + #13#10#13#10 +
          'Windows uses whichever folder comes first, so that copy may be the one that runs. ' +
          'Remove the one you don''t want; this installer has left it alone.', mbInformation, MB_OK);
      Exit;
    end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    AddToPath(ExpandConstant('{app}'));
    WarnOtherHydra();
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
```

- [ ] **Step 4: Build it locally**

```bash
HYDRA_VERSION=0.6.9 cargo build --release -p hydra-cli -p hydraui
mkdir -p dist
cp target/release/hydra.exe dist/hydra-x86_64-pc-windows-msvc.exe
cp target/release/hydraui.exe dist/hydraui-x86_64-pc-windows-msvc.exe
"/c/Program Files (x86)/Inno Setup 6/ISCC.exe" //Qp //DAppVersion=0.6.9 "//DDistDir=$(cygpath -w "$PWD/dist")" installer/hydra.iss
```

(Use the `ISCC.exe` path the local Inno Setup install reports; Git Bash needs `//` for `/` switches.)
Expected: `dist/hydra-setup-x86_64.exe` exists. Add `dist/` to `.gitignore` if it isn't ignored already.

- [ ] **Step 5: Run the test**

Run: `pwsh installer/test-installer.ps1 -Setup dist/hydra-setup-x86_64.exe -Version 0.6.9`
Expected: every line `ok`, then `all checks passed`. Then rebuild without `HYDRA_VERSION` so local exes are `-dev` again.

- [ ] **Step 6: Commit**

```bash
git add installer .gitignore
git commit -m "feat: hydra installer (Inno Setup), with a silent install/uninstall test"
```

---

### Task 2: hydraui keeps the installed version current after an update

**Files:**
- Modify: `crates/hydraui/src/updater.rs` (`record_installed_version`, `installed_here`), `crates/hydraui/src/workspace/title_bar.rs` (call it after a successful install)
- Test: `crates/hydraui/src/updater.rs`

**Interfaces:**
- Produces: `pub const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C3F6B1E-5A2D-4E7B-9F10-3D2C6A4B7E91}_is1";`; `pub fn installed_here(install_location: Option<&str>, exe: &Path) -> bool`; `pub fn record_installed_version(exe: &Path, version: &semver::Version)` (reads `InstallLocation` with `reg query`, writes `DisplayVersion` with `reg add` when `installed_here`; hidden windows; errors ignored).

- [ ] **Step 1: Write the failing test** (in `updater.rs` tests)

```rust
    #[test]
    fn only_the_installed_copy_records_its_version() {
        let exe = Path::new(r"C:\Users\u\AppData\Local\Programs\hydra\hydraui.exe");
        assert!(installed_here(Some(r"C:\Users\u\AppData\Local\Programs\hydra\"), exe));
        assert!(installed_here(Some(r"c:\users\U\appdata\local\programs\HYDRA"), exe));
        assert!(!installed_here(Some(r"C:\Users\u\AppData\Local\Programs\hydra\"), Path::new(r"C:\Users\u\Downloads\hydraui.exe")));
        assert!(!installed_here(None, exe), "not installed at all");
    }
```

- [ ] **Step 2: Run it to see it fail** — `cargo test -p hydraui --lib updater` → compile error (`installed_here` missing).

- [ ] **Step 3: Implement** (in `updater.rs`)

```rust
/// The installer's uninstall key (spec §7.7): Settings → Apps reads `DisplayVersion` here.
pub const UNINSTALL_KEY: &str =
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C3F6B1E-5A2D-4E7B-9F10-3D2C6A4B7E91}_is1";

/// Whether `exe` runs from the installer's folder (`InstallLocation`), however it's spelled.
pub fn installed_here(install_location: Option<&str>, exe: &Path) -> bool {
    let (Some(location), Some(dir)) = (install_location, exe.parent()) else {
        return false;
    };
    let spell = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    spell(location) == spell(&dir.to_string_lossy())
}

fn reg(args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("reg");
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let out = hydra_platform::process::hide_window(&mut cmd).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// After an update, sets the version Settings → Apps shows, when this copy is the installed
/// one; does nothing otherwise, and ignores failures.
pub fn record_installed_version(exe: &Path, version: &Version) {
    let location = reg(&["query", UNINSTALL_KEY, "/v", "InstallLocation"]).and_then(|out| {
        out.lines()
            .find(|l| l.trim_start().starts_with("InstallLocation"))
            .and_then(|l| l.split("REG_SZ").nth(1))
            .map(|v| v.trim().to_string())
    });
    if installed_here(location.as_deref(), exe) {
        let v = version.to_string();
        let _ = reg(&["add", UNINSTALL_KEY, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", &v, "/f"]);
    }
}
```

In `title_bar.rs`, in `start_update`'s background closure, after `updater::install(...)` returns `Ok(done)`, call `updater::record_installed_version(&exe, &done.version);` (clone `exe` into the closure before it's moved, and do it inside the background task so `reg` never runs on the UI thread):

```rust
                .background_spawn(async move {
                    let hydra = updater::find_hydra(
                        &exe,
                        std::env::var_os("PATH").as_deref(),
                        &updater::is_our_hydra,
                    );
                    let result = updater::install(
                        &hydra_update::latest_url(),
                        &exe,
                        hydra.as_deref(),
                        &hydra_update::running_hydra_pids,
                    );
                    if let Ok(done) = &result {
                        updater::record_installed_version(&exe, &done.version);
                    }
                    result
                })
```

- [ ] **Step 4: Run** — `cargo test -p hydraui --lib updater` → passes.

- [ ] **Step 5: Gate and commit** — `git commit -m "feat: hydraui updates the installed version after an in-app update"`

---

### Task 3: The release builds, tests and ships the installer

**Files:**
- Modify: `.github/workflows/ci.yml` (release job)

- [ ] **Step 1: Add two steps** after "Check hydraui's version" and before "Publish the release":

```yaml
      - name: Build the installer
        if: steps.decide.outputs.release == 'true'
        shell: pwsh
        env:
          VERSION: ${{ steps.decide.outputs.version }}
        run: |
          $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
          # Preinstalled on windows-latest; install it if an image ever drops it.
          if (-not (Test-Path $iscc)) { choco install innosetup -y --no-progress | Out-Host }
          & $iscc /Qp "/DAppVersion=$env:VERSION" "/DDistDir=$PWD\dist" installer\hydra.iss
          if ($LASTEXITCODE) { exit $LASTEXITCODE }
          $hash = (Get-FileHash dist\hydra-setup-x86_64.exe -Algorithm SHA256).Hash.ToLower()
          Set-Content -NoNewline -Path dist\hydra-setup-x86_64.exe.sha256 -Value "$hash  hydra-setup-x86_64.exe`n"
          Get-Content dist\hydra-setup-x86_64.exe.sha256
      - name: Test the installer
        if: steps.decide.outputs.release == 'true'
        shell: pwsh
        env:
          VERSION: ${{ steps.decide.outputs.version }}
        run: ./installer/test-installer.ps1 -Setup dist/hydra-setup-x86_64.exe -Version $env:VERSION
```

`gh release create … dist/*` already uploads everything in `dist`, so the installer and its checksum ship with no further change.

- [ ] **Step 2: Check the workflow parses** — `python -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"` → no error; the new steps sit between "Check hydraui's version" and "Publish the release".

- [ ] **Step 3: Commit** — `git commit -m "ci: release builds and tests the installer"`

---

### Task 4: Website and README offer the installer first

**Files:**
- Modify: `site/index.html` (Install section), `site/app.html` (`#start`, hero download), `site/cli.html` (`#install`, hero download), `site/style.css` (portable links), `README.md` (install section)

- [ ] **Step 1: Landing page Install section** (`site/index.html`) — replace the `dl-row` and the two-item list with:

```html
    <div class="dl-row">
      <a class="download" href="https://github.com/davojc/hydra/releases/latest/download/hydra-setup-x86_64.exe" download>Download the installer <span class="dl-version"></span><small>hydra + hydraui</small></a>
    </div>
    <p class="portable">Portable: <a href="https://github.com/davojc/hydra/releases/latest/download/hydraui-x86_64-pc-windows-msvc.exe" download>hydraui.exe</a> · <a href="https://github.com/davojc/hydra/releases/latest/download/hydra-x86_64-pc-windows-msvc.exe" download>hydra.exe</a></p>
    <ul>
      <li><strong>The installer</strong> puts both apps in <code>%LOCALAPPDATA%\Programs\hydra</code> for you alone, with no admin prompt: hydraui in the Start menu, hydra on your <code>PATH</code>, and <code>hydra init</code> done. Uninstall it from Settings → Apps; your environments in <code>~/.hydra</code> stay.</li>
      <li><strong>Portable:</strong> rename to <code>hydraui.exe</code> / <code>hydra.exe</code> and run from anywhere; put <code>hydra.exe</code> on your <code>PATH</code> and run <code>hydra init</code> yourself.</li>
      <li><strong>Updates:</strong> hydraui's title bar shows <strong>Update hydra</strong> when a release is out and updates both; <code>hydra update</code> does the same from the command line.</li>
    </ul>
```

and add to `site/style.css`:

```css
.portable { margin: 0 0 14px; font-size: 15px; color: var(--muted); }
```

- [ ] **Step 2: App guide** (`site/app.html`) — the hero download button becomes the installer (`hydra-setup-x86_64.exe`, label "Download the installer", small "hydra + hydraui") followed by the same `<p class="portable">` line; the first paragraph of `#start` becomes:

```html
      <p>Run the <a href="https://github.com/davojc/hydra/releases/latest/download/hydra-setup-x86_64.exe">installer</a>: it installs hydraui and hydra for you alone, with no admin prompt, adds hydraui to the Start menu and hydra to your <code>PATH</code>. Prefer a single file? The portable <code>hydraui-x86_64-pc-windows-msvc.exe</code> from the <a href="https://github.com/davojc/hydra/releases/latest">latest release</a> runs from anywhere and doesn't need <code>hydra.exe</code>. Either way hydraui reads and writes the same <code>~/.hydra</code> folder as the CLI.</p>
```

- [ ] **Step 3: CLI guide** (`site/cli.html`) — in the hero and `#install`, the download button becomes the installer with the portable line under it; the `#install` paragraph after it becomes:

```html
      <p>The installer puts <code>hydra.exe</code> (and hydraui) in <code>%LOCALAPPDATA%\Programs\hydra</code>, adds it to your <code>PATH</code> and runs <code>hydra init</code>; open a new terminal and <code>hydra</code> is there. For the portable <code>hydra-x86_64-pc-windows-msvc.exe</code> instead, rename it to <code>hydra.exe</code>, put it in a folder on your <code>PATH</code>, then create hydra's home folder:</p>
```

- [ ] **Step 4: README** — the install section leads with the installer (same facts as Step 1, in the README's plain style), then the portable exes, then building from source.

- [ ] **Step 5: Check** — re-run the local link checker (every local `href`/`src` and anchor resolves) and render `index.html` and `app.html` with headless Edge (desktop, and a 375 px iframe) to look at the Install sections.

- [ ] **Step 6: Commit** — `git commit -m "docs(site): installer first, portable exes beside it"`
