# Plan 3: deferred findings and rulings

Collected from the review ledger and the final fix report when Plan 3 finished (2026-10-04). None block merge.

## Rulings
- Ruling: Task 8 is manual (real accounts, real Claude Code) — handed to user — cost: none.
- Ruling: implementers may fix API details if behaviour and exact strings stay; deviations listed — cost: reviewer flags drift.
- Ruling: a broken `.hydra` file is skipped and the walk continues upward (fail-open, matches the guard philosophy) — cost: a broken repo `.hydra` falls back to a parent rule.
- Ruling: wrapper `chain_dir` uses `$(git rev-parse --git-common-dir)/hooks`, not `--git-path hooks` (the latter loops forever once `core.hooksPath` is set) — cost: none.
- Ruling: wrappers block only on exit 1, any other failure allows (spec fail-open) — cost: a crashing guard allows.
- Ruling: git provider is always on; every environment gets the user's effective global config plus the guard hooks, and `[git]` only adds identity overrides — cost: git's "unconfigured = cleared" semantics no longer apply.
- Ruling: shim freshness compares content, not length, so same-width version bumps refresh the shims — cost: small.
- Ruling: unknown or empty bound environment warns "guard skipped" and allows (spec §8) — cost: a typo'd binding guards nothing, but it is warned.
- Ruling: `hydra allow` is scanned as its command in the Claude layer, so only the user can override — cost: Claude can't self-allow (intended).
- Ruling: the `pre-commit install` / `git lfs install` refusals and `git config --global` loss are documented, with the real global file named in the gitconfig header, not fixed in code — cost: users still hit them.

## Deferred findings from the final review
- gh checks in a Claude command line use the hook's cwd, not an earlier `cd` (`Check::Gh` carries no dir), so `cd /x/repo && gh pr create` is checked against the starting folder.
- PowerShell lines go through the bash-style tokenizer: the backtick escape is read as command substitution, and `Set-Location` / `pushd` aren't tracked like `cd`.
- Bare `cd` (home) and `cd -` are not tracked.
- Monorepo repo-root resolution: the git hook resolves the binding at the repo root, so a binding on a sub-folder of a repo isn't seen by `git commit` run there.
- Wrapper cost: every git hook now starts `sh` (about 150 ms per hook on Windows). Fine for commit/push, noticeable for chatty hooks.
- `exclude_in_git` skips worktrees and submodules (where `.git` is a file), so `hydra bind --file` there can't add `.hydra` to the exclude list. It warns.
- p4 (Perforce) hooks and other non-git hook runners aren't wrapped.
- A nested launch from a pre-0.5 terminal loses the global git config: the outer terminal never exported `HYDRA_USER_GIT_CONFIG`, so the inner one only finds `~/.gitconfig`.
- Shim recursion is guarded by directory only: a shim copied elsewhere on `PATH` could recurse. A `HYDRA_SHIM_DEPTH` counter would close it.

## Deferred minor findings (ledger)
- Task 1: `normalize` fixed for `\\?\` prefixes later; remaining: empty env in `.hydra` is accepted at read time; relative dir contract; extra glob tests missing; one test isn't hermetic (walks real `%TEMP%` ancestors).
- Task 2: `config.toml` write isn't atomic; override note wording for `--file`.
- Task 3: `env new --home` isn't atomic; no test for a binding to a deleted env; `set_home` normalises CRLF.
- Task 4: heredoc lines can false-positive (including inside `$()`, an extra warn on the safe side); `ssh://host:22` owner parse fails open; `HEAD@{1}` remote heuristic; wrappers with flags (`sudo -u bob`, `env -i`) aren't unwrapped, a remaining bypass; an unquoted `(` in args truncates the command's args.
- Task 5: `--type=path` expands `~` from `HOME`; `inside()` knows only the current `HYDRA_HOME`; `rename` can fail if a hook runs at that moment (a launch error); `bind_cli` git init inherits the dev `GIT_CONFIG_GLOBAL` (read-only, harmless).
- Task 6: non-UTF-8 args are passed lossily through the shims.
- Task 7: the guard-hook dedupe drops a whole entry if the user grouped hooks with ours; the owner for `git push <named-remote>` uses `origin`; the hook command's quoting breaks if Claude hooks run via PowerShell (fail-open); missing e2e tests (relative cwd).
- Final: PowerShell tool support is best-effort (see above); docs say the guard is not a security boundary.
