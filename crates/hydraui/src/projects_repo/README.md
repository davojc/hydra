# Projects

One folder per project: its spec, milestones, tickets, decisions, meeting notes and resources.
hydraui writes this repo; change projects in hydraui's Projects tab rather than here.

## Who may merge

The `projects-gate` check (`.github/workflows/projects-gate.yml`) decides, from the default
branch:

- **Anyone may start a project:** a new folder merges when its `project.toml` names the pull
  request's author as `lead`.
- **A project's lead changes it freely:** their own pull requests merge at once.
- **Anyone else's change to a project waits for its lead's approval** of the latest commit.
  The lead is always read from the default branch, so a change that writes someone else in
  as lead counts for nothing until the current lead approves it. That's how a project is
  handed over.
- **Everything outside the project folders** (this README, `.github/`) needs an admin's
  review, through `.github/CODEOWNERS`.

## Turning it on (once, by a repo admin)

The check only protects the repo once the default branch requires it:

1. **Settings → General → Pull Requests:** allow squash merging, allow auto-merge, and
   automatically delete head branches.
2. **Settings → Rules → Rulesets → New branch ruleset**, for the default branch, active, with
   no bypass list:
   - restrict deletions and block force pushes;
   - require a pull request before merging, with 0 approvals, and **require review from
     Code Owners**;
   - **require status checks to pass:** `projects-gate`.

   Or create the same ruleset in one go:

   ```bash
   gh api repos/OWNER/REPO/rulesets --method POST --input .github/projects-ruleset.json
   ```
3. Check that the owners in `.github/CODEOWNERS` are right and have write access.

From then on nobody, hydraui included, can merge a change the check refuses or push to the
default branch directly. hydraui's merges wait for the check: a lead's own change merges as
soon as it passes.
