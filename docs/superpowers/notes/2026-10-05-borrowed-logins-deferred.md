# Borrowed logins: deferred findings and rulings

Plan: `docs/superpowers/plans/2026-10-05-hydra-borrowed-logins.md`. Released as 0.6.0.

## Rulings

- `[github]` `owners`/`strict` are guard policy and stay per environment. They are allowed next to `from`, and are not inherited from the owner.
- The guard and the ssh shim read only the environment's own env.toml, so a broken owner can't weaken them.
- `hydra whoami` reports a broken borrow on that tool's line and carries on.
- A Claude sign-in made with an `[env]` API key isn't borrowed; only the folder is. This is documented.
- The generated CLAUDE.md names the owner, so the shared file stays identical for owner and borrower.

## Deferred (minor)

**Config and borrow resolution**
- `borrow.rs` re-implements `read_toml`'s BOM and NotFound handling instead of sharing it.
- A non-string `from` in the owner reads "borrows it from ?".
- An owner whose `[tool]` isn't a table is reported as "has no [tool]".
- Validation errors in the owner's section are attributed to the borrower's env.toml path.
- In the guard path, `keep_own` drops unknown keys next to `from`. Launch still rejects them.

**Removing and renaming**
- `hydra remove` refuses an owner as "lends" even when its section was deleted by hand.
- `borrowers()` skips a borrower whose env.toml doesn't parse, so it doesn't block `env rm` of the owner. That borrower then fails closed.
- `env rm`'s refusal fix line names only the first borrower's first tool.

**Smaller items**
- `add --from` over an inline-table section moves it to a normal table at the end.
- The `gh` → `github` special case lives in `Ctx::provider_dir`.
- The sign-in note's fallback branch ("fix it there") is unreachable today and untested. Its two warn arms are near-duplicates.
- On the site, the `->` line is shown undimmed.
