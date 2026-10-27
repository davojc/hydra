//! A pipeline's branch rule (orchestrator spec, phase 2 addendum): every repository's work for a
//! ticket is on the branch the pipeline names (`branch = "{ticket}"` → `INTH-166`), so the
//! feature environment that spins up from it has the expected name. A stage's report is
//! refused while a pull request in it is on another branch.

use std::path::Path;

use serde_json::Value;

/// GitHub pull request URLs in `text` (`https://github.com/<owner>/<repo>/pull/<n>`), once each.
pub fn pull_requests(text: &str) -> Vec<String> {
    const START: &str = "https://github.com/";
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(START) {
        let tail = &rest[at + START.len()..];
        let parts: Vec<&str> = tail.splitn(4, '/').collect();
        if let [owner, repo, "pull", n] = parts.as_slice() {
            let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
            let url = format!("{START}{owner}/{repo}/pull/{digits}");
            if !digits.is_empty() && plain(owner) && plain(repo) && !out.contains(&url) {
                out.push(url);
            }
        }
        rest = tail;
    }
    out
}

/// The pull requests a stage's report names, in its summary, evidence and handoff, once each.
pub fn report_prs(args: &Value) -> Vec<String> {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let mut texts = vec![text(&args["summary"])];
    if let Some(evidence) = args["evidence"].as_array() {
        texts.extend(evidence.iter().map(|e| text(&e["detail"])));
    }
    texts.push(text(&args["handoff"]));
    let mut out: Vec<String> = Vec::new();
    for url in texts.iter().flat_map(|t| pull_requests(t)) {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// Whether every one of `prs` is on `expected`, asking `branch_of` for each; why not, as the
/// stage session is told it.
pub fn check(
    expected: &str,
    prs: &[String],
    branch_of: impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    for url in prs {
        match branch_of(url) {
            Ok(b) if b == expected => {}
            Ok(b) => {
                return Err(format!(
                    "the pull request {url} is on branch {b}, and this pipeline's branches must \
                     be named exactly {expected}. Push the work to a branch named {expected} in \
                     that repository, open the pull request from it (close the other), then \
                     report again."
                ));
            }
            Err(e) => {
                return Err(format!(
                    "hydraui couldn't read the branch of {url} ({e}), and it must see that it's \
                     {expected}. Check the pull request exists and gh is signed in for this \
                     environment, then report again."
                ));
            }
        }
    }
    Ok(())
}

/// How long a branch lookup may take; past it the report is refused rather than left waiting
/// (a Keychain prompt nobody answers, a network that never replies).
const LOOKUP_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// `cmd`'s output, or why there is none: it couldn't start, or it ran past `limit` and was
/// stopped. Its stdin is closed, so it can't wait on a prompt.
fn output_within(
    mut cmd: std::process::Command,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run hydra: {e}"))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "no answer in {}s (a Keychain prompt waiting?)",
                    limit.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The branch pull request `url` is from, asked of GitHub with `env`'s own `gh` login (`hydra run
/// <env> -- gh pr view`). Blocks, so not on the UI thread.
pub fn gh_branch_of(hydra: &Path, env: &str, url: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new(hydra);
    cmd.args([
        "run",
        env,
        "--",
        "gh",
        "pr",
        "view",
        url,
        "--json",
        "headRefName",
    ])
    .args(["-q", ".headRefName"]);
    let out = output_within(cmd, LOOKUP_LIMIT)?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !stdout.is_empty() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            "gh gave no branch".into()
        } else {
            err
        })
    }
}

/// `gh <args>` as `env`'s own GitHub login (`hydra run <env> -- gh …`): its trimmed output.
/// Blocks, so not on the UI thread.
fn gh(hydra: &Path, env: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new(hydra);
    cmd.args(["run", env, "--", "gh"]).args(args);
    let out = output_within(cmd, LOOKUP_LIMIT)?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !stdout.is_empty() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("gh {} gave nothing", args.join(" "))
        } else {
            err
        })
    }
}

/// Who `env` is on GitHub.
pub fn gh_login(hydra: &Path, env: &str) -> Result<String, String> {
    gh(hydra, env, &["api", "user", "-q", ".login"])
}

/// Who opened pull request `url`.
pub fn gh_author_of(hydra: &Path, env: &str, url: &str) -> Result<String, String> {
    gh(
        hydra,
        env,
        &["pr", "view", url, "--json", "author", "-q", ".author.login"],
    )
}

/// Pull request `url`'s state as GitHub has it: `OPEN`, `MERGED` or `CLOSED`.
pub fn gh_pr_state(hydra: &Path, env: &str, url: &str) -> Result<String, String> {
    gh(
        hydra,
        env,
        &["pr", "view", url, "--json", "state", "-q", ".state"],
    )
}

/// Why a run about a pull request in `state` is over, when it is: merged or closed.
pub fn settled_by(state: &str) -> Option<&'static str> {
    match state.trim() {
        "MERGED" => Some("its pull request was merged"),
        "CLOSED" => Some("its pull request was closed"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_merged_or_closed_pull_request_settles_its_run() {
        assert_eq!(settled_by("MERGED\n"), Some("its pull request was merged"));
        assert_eq!(settled_by("CLOSED"), Some("its pull request was closed"));
        assert_eq!(settled_by("OPEN"), None);
        assert_eq!(settled_by(""), None);
    }

    #[test]
    fn pull_requests_are_found_once_each() {
        assert_eq!(
            pull_requests(
                "PR https://github.com/iov42/version2-frontend/pull/5542, and \
                 https://github.com/iov42/backend/pull/1700/files plus \
                 https://github.com/iov42/version2-frontend/pull/5542 again; not \
                 https://github.com/iov42/backend/issues/3 or https://github.com/x"
            ),
            [
                "https://github.com/iov42/version2-frontend/pull/5542",
                "https://github.com/iov42/backend/pull/1700",
            ]
        );
    }

    #[test]
    fn a_reports_prs_come_from_its_summary_evidence_and_handoff() {
        let args = json!({"summary": "Opened https://github.com/o/fe/pull/1.",
            "evidence": [{"kind": "ci", "detail": "green on https://github.com/o/be/pull/2"}],
            "handoff": "Review https://github.com/o/fe/pull/1 first."});
        assert_eq!(
            report_prs(&args),
            [
                "https://github.com/o/fe/pull/1",
                "https://github.com/o/be/pull/2"
            ]
        );
        assert!(report_prs(&json!({"summary": "no PR"})).is_empty());
    }

    #[test]
    fn a_pr_on_another_branch_is_refused_with_what_to_do() {
        let prs = vec![
            "https://github.com/o/fe/pull/1".to_string(),
            "https://github.com/o/be/pull/2".to_string(),
        ];
        let on = |url: &str| -> Result<String, String> {
            Ok(if url.ends_with("/1") {
                "INTH-166"
            } else {
                "jordan/inth-166-dutch"
            }
            .into())
        };
        let why = check("INTH-166", &prs, on).unwrap_err();
        for says in [
            "https://github.com/o/be/pull/2",
            "jordan/inth-166-dutch",
            "INTH-166",
        ] {
            assert!(why.contains(says), "{says}: {why}");
        }
        assert_eq!(check("INTH-166", &prs[..1], on), Ok(()));
        assert_eq!(check("INTH-166", &[], on), Ok(()));
    }

    #[test]
    fn a_branch_that_cant_be_read_is_refused_too() {
        let prs = vec!["https://github.com/o/fe/pull/1".to_string()];
        let why = check("INTH-166", &prs, |_| Err("gh: not logged in".into())).unwrap_err();
        assert!(
            why.contains("gh: not logged in") && why.contains("INTH-166"),
            "{why}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_lookup_that_hangs_is_stopped() {
        let mut slow = std::process::Command::new("sleep");
        slow.arg("5");
        let started = std::time::Instant::now();
        let why = output_within(slow, std::time::Duration::from_secs(1)).unwrap_err();
        assert!(why.contains("no answer in 1s"), "{why}");
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        let mut quick = std::process::Command::new("echo");
        quick.arg("INTH-7");
        let out = output_within(quick, std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "INTH-7");
    }
}
