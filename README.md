<img src="assets/hydra.png" alt="" width="96" align="right">

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

> **Status:** early (0.6). Windows 11 with PowerShell 7 and Git for Windows. See [What's not built yet](#whats-not-built-yet).

---

## Install

Download `hydra-x86_64-pc-windows-msvc.exe` from the [latest release](https://github.com/davojc/hydra/releases/latest). Rename it to `hydra.exe` and put it in a folder on your `PATH`. Then:

```powershell
hydra init        # creates ~/.hydra
```

**Updating:** `hydra update` installs the latest release after checking its SHA-256 checksum. `hydra update --check` only reports. It won't update while another hydra terminal is open.

**From source:** install Rust from <https://rustup.rs>, then `git clone https://github.com/davojc/hydra.git` and `cargo build --release`. Local builds report a `-dev` version (e.g. `0.5.0-dev`), so `hydra update` replaces them with the real release. `scripts\deploy.ps1` builds and installs to `E:\Tools`, or another folder with `-Target`.

**Releases:** every merge to `main` that changes more than docs publishes a release. Patch versions are bumped automatically. Minor and major versions come from `Cargo.toml`.

**Guide:** <https://davojc.github.io/hydra/>

## Quick start: two GitHub and Claude accounts

**1. One SSH key per GitHub account.** GitHub won't accept the same key on two accounts. Add each `.pub` file to the matching account under *Settings → SSH and GPG keys*.

```powershell
ssh-keygen -t ed25519 -f $HOME\.ssh\id_ed25519_work     -C "work@example.com"
ssh-keygen -t ed25519 -f $HOME\.ssh\id_ed25519_personal -C "me@example.com"
```

**2. Create the environments.** Each gets its own colour. New environments start with no tools on.

```powershell
hydra env new work
hydra env new personal
```

**3. Turn on the tools each one uses.** `hydra add` only writes configuration; it never runs a tool or signs in.

```powershell
hydra add claude work
hydra add git work --name "Your Name" --email work@example.com --ssh-key ~/.ssh/id_ed25519_work
hydra add github work --owner acme
hydra add git personal --name "Your Name" --email me@example.com --ssh-key ~/.ssh/id_ed25519_personal
hydra add github personal
hydra add claude personal
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
| `git` | Always on, even without `hydra add git`: your global git config is loaded underneath, plus hydra's folder guard (see below). `hydra add git` adds the author, email, SSH key for pushes, optional SSH signing key, and its own Git Credential Manager space for other HTTPS hosts. | `--name`, `--email`, `--ssh-key`, `--signing-key` |
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

## Borrowing a login

If two environments use the same account, let one borrow the tool from the other instead of signing in twice. In `envs/work/env.toml`:

```toml
[claude]
from = "personal"

[github]
from = "personal"
```

Or from the command line: `hydra add claude work --from personal`.

- **The whole tool folder is shared,** not just the credential file: login, history, sessions and settings. Nothing is copied or synced, so a sign-in or refresh in `personal` is seen by `work` straight away.
- **A borrowing section holds only `from`.** The owner's settings are used, so any other key there is an error. The owner must have the same tool turned on without `from`; chains aren't allowed, and an environment can't borrow from itself.
- **Except `[github]` `owners` and `strict`.** They are guard policy, so they stay with each environment: `[github]` may set them next to `from` (`hydra add github work --from personal --owner acme --strict`). The borrower's own values are used, never the owner's; leaving them out means no owner list and not strict.
- **Only the folder is borrowed.** A Claude login made with an API key in the owner's `[env]` (`ANTHROPIC_API_KEY` and the like) isn't borrowed; set it in the borrower's `[env]` too.
- **Sign in from the owner.** If the borrowed tool isn't signed in, the terminal says where:

  ```
  hydra: claude: not signed in - work borrows it from personal
    -> sign in there: hydra shell personal, then claude auth login
  ```

  `hydra whoami` shows the owner next to the account, for example `claude   from personal · you@example.com`.
- **`git` can't be borrowed.** The commit author and SSH key stay per environment. Git over HTTPS signs in through `gh`, so borrowing `github` covers it.
- **Guards still compare environment names.** A folder bound to `personal` still blocks `git commit` from a `work` terminal, even if `work` borrows personal's GitHub login. The guards and the SSH key read only the borrower's own env.toml.
- **Editing the owner can break borrowers.** If `hydra env edit personal` removes a tool `work` borrows, `work` fails closed on its next launch and says why; its guard keeps working meanwhile, and `hydra whoami` shows the problem on that tool's line.
- **Removing and renaming are safe for owners.**
  - `hydra remove <tool> work` on a borrower removes the section and leaves the owner's login alone.
  - `hydra remove <tool> personal` refuses while another environment borrows that tool from it, and names them.
  - `hydra env rm personal` refuses while any environment borrows from it (`work borrows claude, github from personal`).
  - `hydra env rename personal home` rewrites `from = "personal"` in every borrower and lists the ones it changed. It refuses while a borrower's terminal is open.

## Folders and guards

A folder can belong to an environment. hydra then opens the right terminal there, and blocks `git commit`, `git push` and `gh` write commands (`gh pr create`, `gh repo delete`, `gh api -X POST` and the like) when you run them from a terminal for a different environment. It applies to commands typed by you and to commands run by Claude Code.

**Bind a folder.** There are two ways:

```powershell
hydra bind E:\work work           # a rule in config.toml: everything under E:\work
hydra bind --file work            # or a .hydra file in the current folder
hydra bind                        # show which environment this folder resolves to
hydra bind --list                 # list the rules in config.toml
hydra unbind E:\work              # remove the rule and/or .hydra file for that folder
```

`--file` writes `.hydra` (`env = "work"`) and adds it to the repo's `.git/info/exclude`, so it is never committed. You can also write the rules by hand in `~/.hydra/config.toml`:

```toml
[bindings]
"E:/work/**" = "work"
"E:/personal/**" = "personal"
```

The most specific binding wins. A `.hydra` file beats a rule at the same or shallower depth, and among rules the longest literal prefix wins. A folder that matches nothing is unbound, and nothing is blocked there.

**Open the right terminal.** `hydra shell` with no name picks the environment from the current folder. `hydra shell work` opens in the environment's `home` folder (set `home = "E:/work"` in `env.toml`) when you start it from somewhere that isn't bound to it. `hydra env new work --home E:\work` sets `home` and binds the folder in one step. `hydra whoami` shows a `folder` line with the binding and whether it matches the terminal.

**What a block looks like:**

```
[personal] PS> git push
hydra: blocked git push - this folder belongs to work (rule E:/work/**), this terminal is personal
  -> open a work terminal here: hydra shell work
  -> or run it once anyway: hydra allow -- git push
```

`hydra allow -- <command>` runs that one command past the guard. When Claude Code is blocked, it is told to ask you to run `hydra allow` in your own terminal, and it can't run `hydra allow` itself.

**How it's enforced.** Three layers, because no single hook covers every way a command can run:

1. **git hooks.** Each environment's generated gitconfig sets `core.hooksPath` to hydra's wrappers in `state/<env>/git/hooks`. The `pre-commit` and `pre-push` wrappers ask hydra first. Every wrapper then runs the hook your repo (or your own global `core.hooksPath`) would have run, so existing hooks keep working.
2. **`gh` and `ssh` shims** in `~/.hydra/shims`, put first on `PATH` in hydra terminals. `gh` is checked and then run. `ssh` adds the environment's key.
3. **A Claude Code hook.** Each environment's `settings.json` gets a `PreToolUse` hook on the `Bash` and `PowerShell` tools. It reads the command Claude is about to run, follows `cd` and `git -C`, and blocks it if the folder belongs to another environment. It also catches `--no-verify`.

If a binding names an environment that doesn't exist, hydra prints `guard skipped` and allows the command.

**Git is always on.** Every environment gets hydra's gitconfig, even without `hydra add git`. It includes your real global config (`GIT_CONFIG_GLOBAL` if you set it, else `~/.config/git/config` and `~/.gitconfig`) plus the guard. The Credential Manager namespace is written only with `[git]`, and the `gh` credential helper only with `[git]` or `[github]`.

**Limits.** The guard stops mistakes. It isn't a security boundary.

- `git commit --no-verify` and `git push --no-verify` skip git hooks outside Claude Code. (The Claude hook catches them.)
- A repo that sets its own `core.hooksPath` (Husky, for example) replaces hydra's hooks, so the git guard is off there. `hydra whoami` says so. The `gh` shim and the Claude hook still apply.
- Inside a hydra terminal, `pre-commit install` refuses and `git lfs install` complains, because `core.hooksPath` is set. Run them from a normal terminal.
- `git config --global` inside hydra edits the generated file, and hydra rewrites it on the next launch. Edit your real global file instead. The generated file's header names it.
- The git hook looks up the binding at the repo root, so one `.hydra` or rule at the root of a repo covers all of it.
- Git Bash's own `ssh` inside Claude Code isn't shimmed. PowerShell and hydra's own Git Bash terminals are.

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
- **Folder guard hook.** Each environment's `settings.json` also gets a `PreToolUse` hook that blocks `git commit`, `git push` and `gh` write commands in folders bound to another environment (see [Folders and guards](#folders-and-guards)). It is added after any hooks of your own.

## Commands

Output is coloured in terminals; set NO_COLOR=1 to turn it off.

| Command | |
|---|---|
| `hydra init` | Create `~/.hydra` |
| `hydra env new <name> [--home DIR]\|list\|edit\|rm\|rename` | Manage environments. `--home` sets where its terminals open and binds that folder. `rename` updates bindings, secrets, saved logins and borrowers too. |
| `hydra add [<tool>] [<env>] [flags]` | Turn a tool on: writes settings only, never signs in. `--from <env>` borrows the tool from another environment (see [Borrowing a login](#borrowing-a-login)). |
| `hydra remove <tool> [<env>]` | Turn a tool off; saved logins are kept |
| `hydra shell [<env>] [--shell pwsh\|bash] [--cwd DIR]` | Open a terminal in an environment. With no name, the environment comes from the current folder's binding. |
| `hydra run <env> -- <command…>` | Run one command in an environment |
| `hydra auth <tool> <env>` | Run a tool's sign-in inside an environment without opening a terminal |
| `hydra secret set\|rm <env>/<key>` | Store or remove a secret (value read from a hidden prompt or piped stdin) |
| `hydra bind [<path>] <env> [--file]` | Bind a folder to an environment (a rule in `config.toml`, or a `.hydra` file with `--file`) |
| `hydra bind [--list]` | Show the current folder's binding, or list every rule |
| `hydra unbind [<path>]` | Remove a folder's rule and/or `.hydra` file |
| `hydra allow -- <command…>` | Run one command past the folder guard |
| `hydra whoami [--env <env>]` | Show which account each tool is actually using, and the current folder's binding |
| `hydra update [--check] [--force]` | Install the latest release from GitHub (checksum-verified) |

## Files

```
~/.hydra/
  config.toml          global settings and [bindings]
  envs/<env>/          yours: env.toml, plus optional claude/ overrides
  state/<env>/         hydra's: each tool's folder, sign-ins, generated config
  state/<env>/git/hooks/  hydra's git hook wrappers (the guard)
  shims/               gh.exe and ssh.exe copies of hydra, first on PATH in hydra terminals
  secrets.toml         names (never values) of stored secrets
```

`[bindings]` in `config.toml` maps folders to environments:

```toml
[bindings]
"E:/work/**" = "work"
"E:/personal/**" = "personal"
```

`HYDRA_HOME` moves `~/.hydra`. Deleting `state/<env>` resets an environment, but you'll need to sign in again.

## Troubleshooting

| Message | What to do |
|---|---|
| `hydra: <tool>: not signed in - …` | Sign in with that tool's own command inside the environment's terminal. If it says `work borrows it from personal`, sign in from the owner: `hydra shell personal`. |
| `hydra: work borrows [x] from y, but y has no [x]` | Turn it on in the owner: `hydra add x y`. |
| `hydra: work borrows [x] from y, which borrows it from z` | Borrowing doesn't chain. Borrow from the owner: `from = "z"`. |
| `secret work/x isn't set` | `hydra secret set work/x` |
| `…id_ed25519_work not found` | Fix the key path: `hydra add git work --ssh-key <path>` |
| `CLAUDE_CONFIG_DIR` error when opening | Remove `CLAUDE_CONFIG_DIR` from `[env]`; `[claude]` sets it. Then sign in again. |
| `… is a real folder, not a link` | Move anything you want to keep into `~/.claude`, delete that folder, and reopen the terminal. |
| A Claude setting you changed disappeared | Your version is in `settings.json.bak`. Put lasting changes in `~/.claude/settings.json` or `envs/<env>/claude/settings.json`. |
| `hydra: blocked git push - this folder belongs to work …` | The folder is bound to another environment. Open the right terminal (`hydra shell work`), or run it once with `hydra allow -- git push`. `hydra bind` shows which rule applies. |
| `hydra: warning: guard skipped (… names unknown environment …)` | A binding names an environment that doesn't exist, so nothing was checked. Fix the name in `config.toml` or the `.hydra` file, or create the environment. |
| `whoami` says the repo sets its own `core.hooksPath` | Husky or similar replaces hydra's git hooks in that repo, so the git guard is off there. The `gh` shim and Claude hook still guard it. |
| `gh/ssh shims not refreshed` | hydra couldn't copy itself into `~/.hydra/shims`, usually because a `gh` or `ssh` from there is running. Close it and open a new terminal. |
| `whoami` shows the wrong account | The browser was signed in to the other account. Sign out inside that environment's terminal and sign in again. |

## What's not built yet

- **`scp` / `sftp`** using the environment's key outside git (`ssh` is handled by the shim).
- **Windows Terminal profiles,** one coloured tab per environment.
- **`hydra doctor`.**
- **macOS and Linux.**

The design is in [`docs/superpowers/specs/2026-10-03-hydra-design.md`](docs/superpowers/specs/2026-10-03-hydra-design.md) and the roadmap in [`docs/superpowers/plans/2026-10-03-hydra-roadmap.md`](docs/superpowers/plans/2026-10-03-hydra-roadmap.md).
