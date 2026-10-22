# Backlog

Open improvements and clean-ups, in one place. Each line says why it matters. The detailed
findings behind the older items, with the rulings made at the time, stay in the notes files
linked under each heading. None of these block a release.

Take an item off when it's done, or move it to "Decided against" with the reason.

## Guard (the folder guard and Claude Code hook)

Detail: [plan 3 deferred](superpowers/notes/2026-10-04-plan-3-deferred.md).

- **`gh` is checked against the folder a command started in.** `cd /x/repo && gh pr create`
  in a Claude command line is checked against the starting folder, not `/x/repo` (`Check::Gh`
  carries no folder). The gap most likely to let a command run as the wrong account.
- **PowerShell commands go through a bash-style parser.** The backtick escape reads as command
  substitution, and `Set-Location` / `pushd` aren't followed like `cd`.
- **Bare `cd` (home) and `cd -` aren't followed.**
- **Wrappers aren't unwrapped:** `sudo -u bob`, `env -i` and the like are a way past the guard.
- **Monorepos:** a binding on a sub-folder isn't seen by `git commit` run there, because the
  hook resolves the binding at the repository root.
- **Every git hook starts `sh`** (about 150 ms each on Windows): fine for commit and push,
  noticeable for chatty hooks.
- **Perforce and other non-git hook runners aren't wrapped.**
- `exclude_in_git` skips worktrees and submodules (`.git` is a file there), so
  `hydra bind --file` can't add `.hydra` to their exclude list; it warns.
- A nested launch from a pre-0.5 terminal loses the global git config (`HYDRA_USER_GIT_CONFIG`
  was never exported).
- Heredoc lines can give false positives; `ssh://host:22` owner parsing fails open; an unquoted
  `(` in arguments truncates them; the owner for `git push <named-remote>` assumes `origin`.

## hydra CLI and environments

Detail: [plan 1 deferred](superpowers/notes/2026-10-03-plan-1-deferred.md),
[plan 2 deferred](superpowers/notes/2026-10-04-plan-2-deferred.md),
[borrowed logins deferred](superpowers/notes/2026-10-05-borrowed-logins-deferred.md).

- **Writes that aren't atomic:** the secrets index (and the credential is written before it),
  `config.toml`, and `env new --home`. A crash mid-write can leave them out of step.
- **A `hydra doctor` / clean-up command.** Parked jobs waiting for it: the stale gws
  `credentials.json` left after `[gws]` is removed, and the ruling that one broken provider
  blocks `whoami` / `auth` for the others.
- **`env rm` and `rename` partial failures:** `remove` deletes secrets before the folder (half
  done if the folder is locked); rename's recovery message understates what was done.
- **Borrowed logins, smaller items:** error text naming the borrower's env.toml for an owner's
  mistake; "borrows it from ?" for a non-string `from`; `env rm`'s refusal names only the first
  borrower; `borrowers()` skips a borrower whose env.toml doesn't parse.
- **Untested paths** listed in the plan 1 and 2 notes (materialise failures, `--cwd`,
  `--shell bash`, Claude file sync edge cases).
- Shim recursion is guarded by folder only; a `HYDRA_SHIM_DEPTH` counter would close it.

## hydraui

- **Accounts popover: a form for tools that need values.** "Add account…" signs in to claude,
  github, azure, gcloud and codex and borrows from other environments; git (name, email, SSH
  key), aws, gws, gemini and kube still need `hydra add`. The model is
  `hydraui::accounts::{AddChoice, add_choices}` and `Hydra::add_tool`. Opening it from a tab's
  right-click menu was set aside, not rejected.
- **The modern input editor.** Its own spec, not written yet; hydraui already reports the
  prompt marks it would build on (OSC 133).
- **Sign-in URLs that a program breaks itself.** Plain URLs are clickable across rows the
  terminal wrapped; if Claude Code's sign-in screen splits a URL with its own line breaks, only
  one piece is clickable. Check during a real sign-in before changing anything.
- **Not yet tried in the app:** Reveal in Explorer and New folder… in the file tree, and the
  gws account line for a signed-in environment (its fields were read from the gws program, not
  seen live).
- **Terminal splits.** Removed with environment tabs; the user wants to decide how splitting
  should behave before it returns.
- Rows in the file tree lost their hover hints (they covered the right-click menu); the hints
  are the root line's tooltip now. A status-line hint might be easier to find.

## Platform

- **The Mac build: what's left.** hydra and hydraui build, test and run on macOS (Apple
  silicon): zsh, the login Keychain, process groups instead of Job Objects, a lock file
  instead of the named mutex, Cmd keys, and macOS builds in each release. Still to do:
  confirm on a real Mac that Claude Code and gh keep each environment's login apart now
  that they keep tokens in the Keychain; signing and notarising; a `.app` bundle (so
  `.hydraui` files open with it); and a Homebrew tap in place of `hydra-setup`.

## Out of scope (decided, not pending)

From the [hydraui spec](superpowers/specs/2026-10-17-hydraui-design.md): GUI editors for
environments, restoring scrollback after a restart, dragging files in the file tree,
command blocks, AI features and team features.

## Decided against

Nothing yet.
