# hydra: borrowed logins

Date: 2026-10-05. Status: approved design, awaiting spec review.
Extends `2026-10-03-hydra-design.md` (§3 concepts, §5 providers, §8 errors, §9 CLI).

## 1. Purpose

Some accounts are shared between environments. Today each environment keeps
its own login per tool, so the user signs in to the same account two or three
times, and again in each one when it expires.

An environment can **borrow** a tool from another environment, its **owner**.
The borrower then uses the owner's tool folder directly: one login, one
refresh, seen by every environment that borrows it, even while they run.

### Success criteria

- `hydra add claude work --from personal` makes a `work` terminal use
  personal's Claude login, history and settings, with no second sign-in.
- Signing in again in the owner fixes every borrower; nothing is copied or
  synced.
- When a borrowed tool isn't signed in, the message names the owner and how to
  sign in there.
- Removing or renaming the owner can't silently break a borrower.

## 2. Scope

- Borrowable: every provider section except `[git]` (`[claude]`, `[github]`,
  `[codex]`, `[gemini]`, `[aws]`, `[azure]`, `[gcloud]`, `[gws]`, `[kube]`).
  The user asked for GitHub, Claude, codex and gemini; the rest come for free
  because the mechanism is the same.
- Not borrowable: `[git]`. Commit author, SSH key and signing key stay per
  environment. git over HTTPS signs in through gh, so borrowing `[github]`
  covers it.
- Whole-folder sharing only. A borrower shares the owner's history, sessions
  and settings for that tool, not only the credential file (user decision:
  login-only copying is fragile and only spreads on the next launch).
- Not in scope: named shared logins owned by no environment; detecting
  expiry while a tool runs (the tool reports that in its own words).

## 3. Config

```toml
# envs/work/env.toml
[claude]
from = "personal"

[github]
from = "personal"
```

- `from` names the owner environment. A section with `from` has no other
  keys; anything else is a config error naming the key (the owner's settings
  are the ones used).
- The owner must have the same section without `from`. If it is missing, or
  itself borrows (no chains), or the owner environment doesn't exist, the
  borrower's config is invalid. Launch fails closed (spec §8), naming the
  owner and the fix:

  ```
  hydra: work borrows [claude] from personal, but personal has no [claude]
    -> add it there: hydra add claude personal
  ```

  Chain error:

  ```
  hydra: work borrows [claude] from client, which borrows it from personal
    -> borrow from the owner: from = "personal"
  ```

- An environment can't borrow from itself.
- Secret references (`secret:personal/gemini`) already name their
  environment, so the owner's `api_key` and `credentials` values resolve
  unchanged in the borrower.

## 4. Behaviour

### 4.1 Resolution

Loading `work` replaces each borrowed section with the owner's section, and
records, per tool, which environment's state folder that tool uses (`work`
by default, `personal` when borrowed). Providers ask for "this tool's state
folder" instead of building `state/<env>/<tool>` themselves. All other
per-environment behaviour (variable names, `HYDRA_ENV`, the guard, bindings)
still uses `work`.

Claude: the borrower's launch prepares the owner's folder exactly as an
owner launch would (base links, merged settings, the guard hook). The guard
hook reads `HYDRA_ENV` at run time, so it still guards as `work`.

### 4.2 Not signed in

The provider's usual note (`not signed in - run claude auth login`) becomes,
for a borrowed tool:

```
hydra: claude: not signed in - work borrows it from personal
  -> sign in there: hydra shell personal, then claude auth login
```

Signing in from the borrower also works (it writes to the owner's folder);
the message just points to the owner, where the login belongs.

### 4.3 whoami

A borrowed tool shows its owner next to the account:

```
claude   from personal   you@example.com
```

### 4.4 Managing environments

- `hydra add <tool> <env> --from <owner>` writes `[<tool>]\nfrom = "<owner>"`,
  with the same validation as §3. `--from` with `git` is refused.
- `hydra remove <tool> <env>` on a borrower removes the section and never
  touches the owner's folder.
- `hydra remove <tool> <owner>` refuses while other environments borrow that
  tool from it, and lists them.
- `hydra env rm <owner>` refuses while any environment borrows from it, and
  lists borrower and tool (`work borrows claude, github`).
- `hydra env rename <owner> <new>` rewrites `from = "<owner>"` in every
  borrower's env.toml (format-preserving) and reports which ones changed.
- `hydra env rm` / `rename` of a borrower need nothing special: a borrower
  has no state folder for a borrowed tool.

## 5. Guards

Unchanged. Guards compare environment names, not accounts. A folder bound to
`personal` still blocks `git commit` from a `work` terminal even if `work`
borrows personal's GitHub login.

## 6. Errors

All errors follow spec §8. Invalid borrowing is a config error, and launch
fails closed. `hydra whoami` reports it on the tool's line and carries on.

## 7. Testing

- Config: a `from`-only section loads; extra keys, a missing owner section,
  self-borrow, chains and an unknown owner are each a clear error.
- Each borrowable provider: the borrower's variables point at the owner's
  state folder (`CLAUDE_CONFIG_DIR`, `GH_CONFIG_DIR`, `CODEX_HOME`, …).
- Claude: a borrower launch prepares the owner's folder, and its guard hook
  still guards as the borrower.
- The not-signed-in message names the owner. whoami shows `from <owner>`.
- `add --from`, `remove` on owner and borrower, `env rm` refusal, and
  `env rename` rewriting borrowers.
- `[git] from` is rejected.
- The existing fences apply: tests never touch the real `~/.claude`,
  `.claude.json` or global git config.

## 8. Docs

README and site: a "Borrowing a login" section, `add --from` in the
commands table, and the new error messages in troubleshooting.
