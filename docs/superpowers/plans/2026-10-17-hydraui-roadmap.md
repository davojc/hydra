# hydraui Roadmap

**Spec:** `docs/superpowers/specs/2026-10-17-hydraui-design.md`

The spec is delivered as four plans. Each later plan is written after the previous one ships, so it can build on what was learned (above all, the GPUI spike in plan 1).

| Plan | Delivers | Spec sections |
|---|---|---|
| **1. Foundation** — `2026-10-17-hydraui-plan-1-foundation.md` | GPUI spike and go/no-go note; the hydra crate changes (guards off, returned warnings, env changes as pairs, shell integration and startup command); the `hydraui-sets` crate (set files, resolution, validation, loaded-set bookkeeping, session file). **Usable result:** `hydra` behaves exactly as before; set files can be validated in tests; the UI stack is confirmed or rejected. | §3, §4, §5, §10 (spike) |
| **2. Terminal core** | `hydraui-term`: ConPTY via portable-pty, Job Objects, per-pane init scripts, `alacritty_terminal` grid on a reader thread, OSC 7/133 parser, `TermEvent` channel, exit and restart. **Usable result:** an integration-tested headless pane that runs a hydra environment and reports cwd and prompt state. | §6, §9 (term tests) |
| **3. App shell** | `hydraui` GPUI app: window, terminal view, tabs, split panes, ad-hoc tabs with environment picker, keybindings, status bar, `config.toml`, pane error banners and warning strip. **Usable result:** a daily-usable multi-tab, multi-pane terminal with per-pane hydra environments. | §7.1, §7.2, §7.5, §7.6 |
| **4. Sets and tools** | Loading/unloading sets in the app (set groups, confirm on unload, reload on change, session restore), launcher palette, sign-in and set-secret actions, file tree, packaging in the release zip. **Usable result:** the full spec. | §5.4 in the app, §7.3, §7.4, §7.7 |

The modern input editor is a separate spec, written after plan 4.
