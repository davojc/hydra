# Plan 2: deferred findings and rulings

Collected from the review ledger when Plan 2 finished (2026-10-04). None block merge.

## Rulings
- Ruling: Task 5 is manual (real Claude accounts, browser login) — handed to user — cost: none.
- Ruling: implementers may fix uncompiled-plan API/borrow details, behaviour unchanged, deviations listed — cost: reviewer flags drift.
- Ruling: Task 4 Minor "Home::hydra() doesn't default HYDRA_USER_HOME" will be fixed in the final fix wave — constraint "tests never touch real ~/.claude" should hold by default, not convention — cost: none
- Ruling: Important 2 handled by backup+warning, not by auto-merging Claude's in-env edits into overrides — explicit beats magic — cost if wrong: users must copy lasting changes by hand
- Ruling: Minor 7 (pre-Plan-2 envs keep sharing ~/.claude; ANTHROPIC_API_KEY cleared) handled as docs/release note by controller — cost: none
- Ruling: Minor 9 (ide/ not shared; plugin data shared via junction) recorded for the manual Task 5 checks; not changed — sharing plugins/ is the spec's intent — cost if wrong: a plugin storing per-user tokens shares them across envs
- Ruling: Minor 10 docs wording fixed by controller (spec /login -> claude auth login; plan byte-for-byte wording)

## Deferred minor findings
- Task 1: minor (deferred): unlink uses remove_dir only on Windows (file symlinks would error, fail-safe); dep ordering cosmetic
- Task 2: minor (deferred): glob_match exponential on pathological many-* patterns (user-authored); sync rewrites file formatting (not byte-identical) and writes empty mcpServers once
- Task 3: minor (deferred): plain file where a link goes gives raw OS error; .claude.json RMW can race a running Claude (only when mcpServers changed, sync is no-op otherwise); stale .hydra-tmp on failed write; missing tests (base byte-identical, CLAUDE.md removal branch, file-in-place)
- Task final: minor (deferred): .bak copies from path not from the read content (rare double-launch race after an in-env edit); README text doesn't mention .bak; claude_base "~user/x" passes validation but isn't expanded
