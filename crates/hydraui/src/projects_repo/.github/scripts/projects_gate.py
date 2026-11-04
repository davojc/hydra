"""The projects repo's merge gate (hydraui Projects, spec section 14.4).

A pull request passes when:
- every file it changes is in a project folder (<slug>/...), README.md or .github/ (the last
  two need an admin's review, through CODEOWNERS);
- each project new on the base branch is led by the pull request's author (the lead in the
  pull request's <slug>/project.toml): anyone can start a project;
- each project already on the base branch is led by the author, or its lead has approved the
  pull request's latest commit. The lead is read from the base branch, never from the change,
  so writing yourself in as lead counts for nothing.

Run by .github/workflows/projects-gate.yml; uses only the standard library.
"""

import base64
import json
import os
import sys
import urllib.error
import urllib.request

TOP_LEVEL_OK = ("README.md",)


def same(a, b):
    return a.lower() == b.lower()


def decide(paths, author, base_leads, head_leads, approved_by):
    """The problems with a pull request; none means it may merge.

    paths: every file it changes (a rename's both names).
    base_leads: {slug: lead} for each touched project the base branch has ("" if unreadable).
    head_leads: {slug: lead} as the pull request writes them, for new projects.
    approved_by: logins whose latest review approves the pull request's latest commit.
    """
    problems = []
    slugs = set()
    for path in paths:
        if path in TOP_LEVEL_OK or path.startswith(".github/"):
            continue
        if "/" not in path:
            problems.append(f"{path}: only project folders, README.md and .github/ go at the top")
            continue
        slugs.add(path.split("/", 1)[0])
    for slug in sorted(slugs):
        if slug in base_leads:
            lead = base_leads[slug]
            if not lead:
                problems.append(f"{slug}: its project.toml on the base branch names no lead")
            elif not same(author, lead) and not any(same(a, lead) for a in approved_by):
                problems.append(f"{slug}: {author} doesn't lead it; its lead, {lead}, must approve")
        else:
            lead = head_leads.get(slug, "")
            if not same(author, lead):
                problems.append(
                    f'{slug}: a new project must be led by whoever adds it (lead = "{author}")'
                )
    return problems


def approvers(reviews, head_sha):
    """Logins whose latest approving-or-not review approves commit head_sha. A comment after
    an approval doesn't undo it."""
    latest = {}
    for r in reviews:
        if r.get("state") in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            latest[r["user"]["login"]] = r
    return {
        login
        for login, r in latest.items()
        if r["state"] == "APPROVED" and r.get("commit_id") == head_sha
    }


def api(path):
    req = urllib.request.Request(
        f"https://api.github.com/repos/{os.environ['GITHUB_REPOSITORY']}/{path}",
        headers={
            "Authorization": f"Bearer {os.environ['GITHUB_TOKEN']}",
            "Accept": "application/vnd.github+json",
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)


def pages(path):
    out, page = [], 1
    while True:
        batch = api(f"{path}{'&' if '?' in path else '?'}per_page=100&page={page}")
        out.extend(batch)
        if len(batch) < 100:
            return out
        page += 1


def lead_at(slug, ref):
    """The lead in <slug>/project.toml at ref: None when there's no such file, "" when it
    doesn't read."""
    import tomllib

    try:
        body = api(f"contents/{slug}/project.toml?ref={ref}")
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise
    try:
        return str(tomllib.loads(base64.b64decode(body["content"]).decode())["lead"])
    except Exception:
        return ""


def main():
    with open(os.environ["GITHUB_EVENT_PATH"]) as f:
        pr = json.load(f)["pull_request"]
    number, author = pr["number"], pr["user"]["login"]
    base, head = pr["base"]["sha"], pr["head"]["sha"]
    paths = []
    for f in pages(f"pulls/{number}/files"):
        paths.append(f["filename"])
        if f.get("previous_filename"):
            paths.append(f["previous_filename"])
    slugs = {p.split("/", 1)[0] for p in paths if "/" in p and not p.startswith(".github/")}
    base_leads, head_leads = {}, {}
    for slug in slugs:
        lead = lead_at(slug, base)
        if lead is None:
            head_leads[slug] = lead_at(slug, head) or ""
        else:
            base_leads[slug] = lead
    approved_by = approvers(pages(f"pulls/{number}/reviews"), head)
    problems = decide(paths, author, base_leads, head_leads, approved_by)
    for p in problems:
        print(f"::error::{p}")
    if problems:
        sys.exit(1)
    print(f"OK: {author} may merge this ({', '.join(sorted(slugs)) or 'no project'}).")


if __name__ == "__main__":
    main()
