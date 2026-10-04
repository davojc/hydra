# hydra

**Terminals with their own identities.** hydra opens PowerShell or Git Bash terminals inside named *environments*. Each environment has its own sign-ins and identity for the tools you use, including Claude Code, the GitHub CLI, git, AWS, Azure, Google Cloud and more. A `work` terminal and a `personal` terminal can then sit side by side, each signed in to a different account, and neither can quietly use the other's.

```
PS> hydra shell work
[work] PS> gh api user --jq .login        # your work GitHub account
[work] PS> git config user.email           # work@example.com
[work] PS> claude                          # Claude Code, signed in as your work account

PS> hydra shell personal
[personal] PS> gh api user --jq .login    # your personal GitHub account
```

Typical uses:

- **Stop identity mix-ups.** Commits and pushes always use the environment's author, SSH key and GitHub login.
- **Run Claude Code on several accounts at once.** Each environment has its own Claude sign-in, usage limits and history, and all of them share your skills, plugins, settings and MCP servers.

> **Status:** early (0.3). Windows 11 with PowerShell 7 and Git for Windows. See [What's not built yet](#whats-not-built-yet).

---

## Install

hydra is a Rust program. Install Rust from <https://rustup.rs>, then:

```powershell
git clone https://github.com/davojc/hydra.git
cd hydra
cargo build --release
```

Copy `target\release\hydra.exe` to a folder on your `PATH`. Alternatively, run `scripts\deploy.ps1`, which builds and installs to `E:\Tools` (pass `-Target <folder>` for another folder) and refuses to install while a hydra terminal is running.

```powershell
hydra init        # creates ~/.hydra
```

## Quick start: two GitHub and Claude accounts

**1. One SSH key per GitHub account.** GitHub won't accept the same key on two accounts. Add each `.pub` file to the matching account under *Settings → SSH and GPG keys*.

```powershell
ssh-keygen -t ed25519 -f $HOME\.ssh\id_ed25519_work     -C "work@example.com"
ssh-keygen -t ed25519 -f $HOME\.ssh\id_ed25519_personal -C "me@example.com"
```

**2. Create the environments.** Each gets its own colour, and Claude is on by default.

```powershell
hydra env new work
hydra env new personal
```

**3. Turn on the tools each one uses.** `hydra add` only writes configuration; it never runs a tool or signs in.

```powershell
hydra add git work --name "Your Name" --email work@example.com --ssh-key ~/.ssh/id_ed25519_work
hydra add github work --owner acme
hydra add git personal --name "Your Name" --email me@example.com --ssh-key ~/.ssh/id_ed25519_personal
hydra add github personal
```

**4. Sign in inside each environment,** with each tool's own command. Terminals open even before you've signed in, and hydra lists what's missing:

```
PS> hydra shell work
hydra: claude: not signed in - run claude auth login
hydra: github: not signed in - run gh auth login
[work] PS> gh auth login          # sign in with the work account
[work] PS> claude auth login      # and the work Claude account
[work] PS> exit
```

Repeat in `hydra shell personal`. Browser sign-ins use whichever account the browser is signed in to, so use a private window for the second account.

**5. Check.**

```
PS> hydra whoami --env work
env      work · work
claude   work@example.com (Acme)                          ok
github   work-user                                        ok
git      work@example.com                                 ok
```

## How it works

hydra starts each terminal with environment variables that point every tool at that environment's own folder under `~/.hydra/state/<env>/`. For example, `CLAUDE_CONFIG_DIR`, `GH_CONFIG_DIR`, `GIT_CONFIG_GLOBAL`, `CLOUDSDK_CONFIG` and `AZURE_CONFIG_DIR`.

- **Inherited variables are cleared.** Identity variables picked up from the terminal you launched from, such as `GH_TOKEN`, `AWS_PROFILE` or `ANTHROPIC_API_KEY`, are removed. This includes those of tools the environment doesn't use, so nothing leaks between environments, even when one hydra terminal is started from inside another.
- **Launch fails closed.** If something could make a tool fall back to another identity, hydra refuses to open the terminal and says how to fix it. Examples are a configured SSH key that doesn't exist, or a secret you referenced but never stored. A tool that simply isn't signed in yet doesn't block anything.
- **Secrets live in Windows Credential Manager.** Config files only reference them (`secret:work/linear`). Values never go to disk or into hydra's output. The one exception is a gws credentials file, which is written readable by your Windows account only.

## Tools

Turn tools on with `hydra add <tool> [env]`. Inside a hydra terminal you can leave out the environment.

| Tool | What each environment gets | `hydra add` flags |
|---|---|---|
| `claude` | Its own Claude Code sign-in, sharing your `~/.claude` setup (see below) | `--mcp-exclude <pattern>` |
| `github` (`gh`) | Its own `gh auth login`; git's HTTPS pushes to github.com use it | `--owner <org>`, `--strict` |
| `git` | Author, email, SSH key for pushes, optional SSH signing key, its own Git Credential Manager space for other HTTPS hosts. Your `~/.gitconfig` is still loaded underneath. | `--name`, `--email`, `--ssh-key`, `--signing-key` |
| `aws` | An AWS profile, optionally with private config and credentials files | `--profile` (required), `--isolate` |
| `azure` | Its own `az login` | |
| `gcloud` | Its own `gcloud auth login` and configurations | |
| `gws` | Its own Google Workspace CLI login, or stored credentials | `--credentials-secret <env/key>` |
| `kube` | Its own kubeconfig | `--kubeconfig <path>` |
| `codex` | Its own Codex CLI login | |
| `gemini` | `GEMINI_API_KEY` from a stored secret | `--api-key-secret <env/key>` (required) |

- `hydra add` with no tool lists tools and which ones are on.
- `hydra remove <tool> [env]` turns one off. Its saved logins are kept.
- Any other variable or secret goes under `[env]` in the environment's `env.toml`:

```toml
[env]
REGION = "eu-west"
LINEAR_API_KEY = "secret:work/linear"   # store it with: hydra secret set work/linear
```

## Claude Code: separate accounts, shared setup

Each environment's Claude folder (`~/.hydra/state/<env>/claude`) is rebuilt from your existing `~/.claude` every time a terminal opens. hydra only ever **reads** `~/.claude` and `~/.claude.json`.

| | |
|---|---|
| **Linked from `~/.claude`** | `skills/`, `commands/`, `agents/`, `hooks/`, `output-styles/`, `plugins/`. Install once and it's available everywhere. |
| **Rebuilt each launch** | `settings.json` (yours merged with the environment's own `envs/<env>/claude/settings.json`), `CLAUDE.md` (yours plus the environment's own section), and the MCP servers from `~/.claude.json`, minus any `--mcp-exclude` patterns |
| **Private to the environment** | the sign-in, history, projects, sessions, and the rest of `.claude.json` |

- **Per-environment changes:** put files in `~/.hydra/envs/<env>/claude/`.
- **Changes made inside Claude:** if Claude itself edits the rebuilt `settings.json` or `CLAUDE.md` (for example with `/model`), hydra saves your version as `.bak` on the next launch, tells you, and rebuilds the file.
- **Sign-in settings are never shared.** `apiKeyHelper` and API-key variables in `~/.claude/settings.json` are left out of every environment, with a warning, so environments can't end up on the same account.
- **Plain `claude` is unchanged.** Running `claude` outside hydra keeps using `~/.claude` as before.

## Commands

| Command | |
|---|---|
| `hydra init` | Create `~/.hydra` |
| `hydra env new\|list\|edit\|rm\|rename` | Manage environments. `rename` updates secrets and saved logins too. |
| `hydra add [<tool>] [<env>] [flags]` | Turn a tool on: writes settings only, never signs in |
| `hydra remove <tool> [<env>]` | Turn a tool off; saved logins are kept |
| `hydra shell [<env>] [--shell pwsh\|bash] [--cwd DIR]` | Open a terminal in an environment |
| `hydra run <env> -- <command…>` | Run one command in an environment |
| `hydra auth <tool> <env>` | Run a tool's sign-in inside an environment without opening a terminal |
| `hydra secret set\|rm <env>/<key>` | Store or remove a secret (value read from a hidden prompt or piped stdin) |
| `hydra whoami [--env <env>]` | Show which account each tool is actually using |

## Files

```
~/.hydra/
  config.toml          global settings
  envs/<env>/          yours: env.toml, plus optional claude/ overrides
  state/<env>/         hydra's: each tool's folder, sign-ins, generated config
  secrets.toml         names (never values) of stored secrets
```

`HYDRA_HOME` moves `~/.hydra`. Deleting `state/<env>` resets an environment, but you'll need to sign in again.

## Troubleshooting

| Message | What to do |
|---|---|
| `hydra: <tool>: not signed in - …` | Sign in with that tool's own command inside the environment's terminal. |
| `secret work/x isn't set` | `hydra secret set work/x` |
| `…id_ed25519_work not found` | Fix the key path: `hydra add git work --ssh-key <path>` |
| `CLAUDE_CONFIG_DIR` error when opening | Remove `CLAUDE_CONFIG_DIR` from `[env]`; `[claude]` sets it. Then sign in again. |
| `… is a real folder, not a link` | Move anything you want to keep into `~/.claude`, delete that folder, and reopen the terminal. |
| A Claude setting you changed disappeared | Your version is in `settings.json.bak`. Put lasting changes in `~/.claude/settings.json` or `envs/<env>/claude/settings.json`. |
| `whoami` shows the wrong account | The browser was signed in to the other account. Sign out inside that environment's terminal and sign in again. |

## What's not built yet

- **Folder bindings and guards:** blocking `git push` / `gh pr create` from a folder that belongs to another environment, including when Claude Code runs it.
- **`hydra shell` without a name:** picking the environment from the current folder.
- **Plain `ssh` / `scp`** using the environment's key outside git.
- **Windows Terminal profiles,** one coloured tab per environment.
- **`hydra doctor`.**
- **macOS and Linux.**

The design is in [`docs/superpowers/specs/2026-10-03-hydra-design.md`](docs/superpowers/specs/2026-10-03-hydra-design.md) and the roadmap in [`docs/superpowers/plans/2026-10-03-hydra-roadmap.md`](docs/superpowers/plans/2026-10-03-hydra-roadmap.md).
