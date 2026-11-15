<img src="site/favicon.png" alt="" width="64" align="right">

# hydra

**Terminals with their own identities.** hydra opens PowerShell or Git Bash terminals (zsh on macOS) inside named *environments*. Each environment has its own sign-ins and identity for the tools you use, including Claude Code, the GitHub CLI, git, AWS, Azure, Google Cloud and more. A `work` terminal and a `personal` terminal can sit side by side, each signed in to a different account, and neither can quietly use the other's.

hydra comes in two parts that share the same `~/.hydra` folder:

- **hydraui**, a desktop app: a tab per environment, its terminals down the side, a file tree and file pane, and a task board that the Claude sessions in those terminals work through.
- **`hydra`**, the command line: it creates environments, turns tools on, opens terminals in your own console, and guards folders against the wrong account.

**Guide:** <https://davojc.github.io/hydra/>

This repository holds hydra's [releases](https://github.com/davojc/hydra/releases) and its guide. The source isn't public.

> **Status:** early. Windows 11 with PowerShell 7 and Git for Windows, and macOS on Apple silicon with zsh (newer still: the builds aren't signed yet).

## Install

**Windows, installer (recommended):** download [`hydra-setup.exe`](https://github.com/davojc/hydra/releases/latest/download/hydra-setup.exe) and run it. It installs hydraui and hydra for you alone, with no admin prompt, into `%LOCALAPPDATA%\Programs\hydra`: hydraui in the Start menu, hydra on your `PATH`, and `hydra init` done. Uninstall from Settings → Apps; `~/.hydra` stays. `hydra-setup.exe --silent` installs with no dialog.

**Windows, portable:** download `hydraui-x86_64-pc-windows-msvc.exe` and/or `hydra-x86_64-pc-windows-msvc.exe` from the [latest release](https://github.com/davojc/hydra/releases/latest) (each has a `.sha256`). Rename them to `hydraui.exe` / `hydra.exe`, put `hydra.exe` in a folder on your `PATH`, then run `hydra init`.

**macOS (Apple silicon):** in Terminal, run

```sh
curl -fsSL https://github.com/davojc/hydra/releases/latest/download/install.sh | sh
```

It downloads the latest **Hydra.app**, checks its SHA-256, puts it in `~/Applications`, links `~/.local/bin/hydra` to the `hydra` inside it (adding `~/.local/bin` to your `PATH` if it isn't there), and runs `hydra init`. Run it again to reinstall. `install.sh` is attached to each release; read it first if you like.

**macOS by hand:** download `hydra-macos-aarch64.zip` (Hydra.app) or the bare `hydraui-aarch64-apple-darwin` / `hydra-aarch64-apple-darwin` from the [latest release](https://github.com/davojc/hydra/releases/latest) (each has a `.sha256`). The builds aren't signed yet, so clear the quarantine your browser adds:

```sh
xattr -dr com.apple.quarantine Hydra.app        # or, for the bare builds:
chmod +x hydra hydraui && xattr -d com.apple.quarantine hydra hydraui
hydra init        # creates ~/.hydra
```

## Updating

`hydra update` installs the latest release after checking its SHA-256 checksum; `hydra update --check` only reports. hydraui shows an **Update** button beside the notifications bell when a release is out, updates both programs the same way, then offers a restart.
