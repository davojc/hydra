# Hydra Roadmap

**Spec:** `docs/superpowers/specs/2026-10-03-hydra-design.md`

The spec is delivered as four plans. Each one ends with software you can use, and each later plan is written after the previous one ships, so it can build on what was learned.

| Plan | Delivers | Spec sections | Spike (§11) |
|---|---|---|---|
| **1. Foundation** — `2026-10-03-hydra-plan-1-foundation.md` | Rust workspace; environments (`env new/list/edit/rm/rename`); secrets; every provider except claude; `hydra shell` / `run` / `auth` / `whoami` with an explicit environment; `hydra init` (without integrations). **Usable result:** separate GitHub, git, cloud and AI-CLI identities per terminal. | §3, §4.1–4.3 (explicit env only), §5 (minus claude), §8, §9 (subset) | 2 (gh separation) |
| **2. Claude** | claude provider: per-env `CLAUDE_CONFIG_DIR`, layering from `base/claude` (links, merged settings/CLAUDE.md, MCP exclude), `hydra import claude`, `hydra auth claude`. **Usable result:** parallel Claude Code sessions on different accounts sharing one setup. | §6, claude row of §5 | 1, 4, 5 |
| **3. Bindings and guards** | binding resolution, `bind` / `unbind`, `hydra shell` without a name, `home`, `env new --home`, shims + guard, `hydra allow`, `hydra install|uninstall shims`, an `ssh` shim so plain `ssh`/`scp` use the environment's key (via a generated ssh config that includes `~/.ssh/config`), whoami folder line, Windows CI integration tests against a bare repo. **Usable result:** wrong-account pushes are blocked, including from Claude Code. | §4.3 (folder/env choice), §7, §9.1 (shims) | 6 |
| **4. Windows Terminal and doctor** | `hydra install|uninstall wt` (fragment, shell detection, stable GUIDs), auto-refresh from `env new/edit/rename/rm`, `hydra init` runs `hydra install`, `hydra doctor`. **Usable result:** coloured per-environment tabs and a self-check. | §9.1 (wt), §10 runtime self-check | 3, 7 |
