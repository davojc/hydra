//! The guard decision: which git/gh commands are guarded, and whether to allow, warn or block.
use crate::bindings::Binding;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// git commit / git push. `dir` is the effective folder (from `-C`), `remote` the push URL if known.
    Git {
        action: GitAction,
        dir: Option<String>,
        remote: Option<String>,
    },
    /// A gh write command; `repo` is the --repo/-R value if given ("owner/name").
    Gh {
        label: String,
        repo: Option<String>,
        checks_owner: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitAction {
    Commit,
    Push,
}

impl Check {
    /// The short command: "git commit", "git push", "gh pr create", ...
    pub fn label(&self) -> String {
        match self {
            Check::Git {
                action: GitAction::Commit,
                ..
            } => "git commit".into(),
            Check::Git {
                action: GitAction::Push,
                ..
            } => "git push".into(),
            Check::Gh { label, .. } => label.clone(),
        }
    }

    fn carries_owner(&self) -> bool {
        match self {
            Check::Git { action, .. } => *action == GitAction::Push,
            Check::Gh { checks_owner, .. } => *checks_owner,
        }
    }
}

/// git argv (without "git"): Some for commit/push. Skips global options.
pub fn classify_git(args: &[String]) -> Option<Check> {
    let mut dir = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-C" => {
                dir = args.get(i + 1).cloned();
                i += 2;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" => i += 2,
            "-P"
            | "--no-pager"
            | "--paginate"
            | "-p"
            | "--no-replace-objects"
            | "--bare"
            | "--literal-pathspecs"
            | "--no-optional-locks" => i += 1,
            _ if a.starts_with("--git-dir=")
                || a.starts_with("--work-tree=")
                || a.starts_with("--namespace=") =>
            {
                i += 1
            }
            _ => break,
        }
    }
    let action = match args.get(i)?.as_str() {
        "commit" => GitAction::Commit,
        "push" => GitAction::Push,
        _ => return None,
    };
    let remote = if action == GitAction::Push {
        args[i + 1..]
            .iter()
            .find(|a| !a.starts_with('-') && (a.contains("://") || a.contains('@')))
            .cloned()
    } else {
        None
    };
    Some(Check::Git {
        action,
        dir,
        remote,
    })
}

/// The value of a flag that takes the next argument or `=value`.
fn flag_value(args: &[String], names: &[&str]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if names.contains(&a.as_str()) {
            return args.get(i + 1).cloned();
        }
        for n in names {
            if let Some(v) = a.strip_prefix(n)
                && let Some(v) = v.strip_prefix('=')
            {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// gh argv (without "gh"): Some for the write commands.
pub fn classify_gh(args: &[String]) -> Option<Check> {
    let group = args.first()?.as_str();
    let repo = flag_value(args, &["-R", "--repo"]);
    if group == "api" {
        let method = flag_value(args, &["-X", "--method"]);
        let writes = method.is_some_and(|m| !m.eq_ignore_ascii_case("GET"))
            || args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-f" | "-F" | "--field" | "--raw-field" | "--input"
                ) || a.starts_with("--field=")
                    || a.starts_with("--raw-field=")
                    || a.starts_with("--input=")
            });
        return writes.then(|| Check::Gh {
            label: "gh api".into(),
            repo,
            checks_owner: false,
        });
    }
    let subs: &[&str] = match group {
        "pr" => &["create", "merge", "edit", "close", "comment", "review"],
        "repo" => &["create", "delete", "edit", "fork", "rename"],
        "release" => &["create", "delete", "edit", "upload"],
        "issue" => &["create", "edit", "close", "comment", "delete"],
        _ => return None,
    };
    let sub = args.get(1)?.as_str();
    subs.contains(&sub).then(|| Check::Gh {
        label: format!("gh {group} {sub}"),
        repo,
        checks_owner: true,
    })
}

fn end_token(tokens: &mut Vec<String>, cur: &mut String, has: &mut bool) {
    if *has {
        tokens.push(std::mem::take(cur));
        *has = false;
    }
}

/// Splits a shell line into commands (on `&&`, `||`, `;`, `|`, `&`, newline outside quotes),
/// each tokenised shell-style.
fn split_commands(line: &str) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<String>> = Vec::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_tok = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has_tok = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                has_tok = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                                cur.push(n);
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        },
                        _ => cur.push(q),
                    }
                }
            }
            ' ' | '\t' | '\r' => end_token(&mut tokens, &mut cur, &mut has_tok),
            '&' | '|' | ';' | '\n' => {
                if (c == '&' || c == '|') && chars.peek() == Some(&c) {
                    chars.next();
                }
                end_token(&mut tokens, &mut cur, &mut has_tok);
                if !tokens.is_empty() {
                    parts.push(std::mem::take(&mut tokens));
                }
            }
            _ => {
                has_tok = true;
                cur.push(c);
            }
        }
    }
    end_token(&mut tokens, &mut cur, &mut has_tok);
    if !tokens.is_empty() {
        parts.push(tokens);
    }
    parts
}

fn is_env_assignment(t: &str) -> bool {
    match t.split_once('=') {
        Some((k, _)) => {
            !k.is_empty()
                && !k.starts_with(|c: char| c.is_ascii_digit())
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Shell command line (Claude's Bash tool input) -> every guarded check in it.
pub fn checks_in_command_line(line: &str) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut cd: Option<String> = None;
    for part in split_commands(line) {
        let tokens: Vec<&String> = part
            .iter()
            .skip_while(|t| is_env_assignment(t.as_str()))
            .collect();
        let Some(prog) = tokens.first() else { continue };
        let lower = prog
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(prog)
            .to_lowercase();
        let base = lower.strip_suffix(".exe").unwrap_or(&lower);
        let rest: Vec<String> = tokens[1..].iter().map(|t| (*t).clone()).collect();
        match base {
            "cd" => {
                if let Some(d) = rest.iter().find(|a| !a.eq_ignore_ascii_case("/d")) {
                    cd = Some(d.clone());
                }
            }
            "git" => {
                if let Some(mut c) = classify_git(&rest) {
                    if let Check::Git { dir, .. } = &mut c
                        && dir.is_none()
                    {
                        *dir = cd.clone();
                    }
                    checks.push(c);
                }
            }
            "gh" => checks.extend(classify_gh(&rest)),
            _ => {}
        }
    }
    checks
}

/// "https://github.com/o/r(.git)", "git@github.com:o/r(.git)", "ssh://git@github.com/o/r" -> Some("o").
pub fn github_owner(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        let (_, rest) = url.split_once("://")?;
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    let owner = path.trim_start_matches('/').split('/').next()?;
    (!owner.is_empty()).then(|| owner.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Warn(String),
    Block(String),
}

pub struct Facts<'a> {
    /// HYDRA_ENV
    pub current_env: Option<&'a str>,
    /// HYDRA_ALLOW=1
    pub allow: bool,
    pub binding: Option<&'a Binding>,
    /// The current env's github.owners.
    pub owners: &'a [String],
    pub strict: bool,
    /// Resolved by the caller.
    pub target_owner: Option<&'a str>,
}

pub fn decide(check: &Check, f: &Facts) -> Verdict {
    let Some(current) = f.current_env else {
        return Verdict::Allow;
    };
    if f.allow {
        return Verdict::Allow;
    }
    let cmd = check.label();
    if let Some(b) = f.binding
        && b.env != current
    {
        return Verdict::Block(format!(
            "hydra: blocked {cmd} - this folder belongs to {bound} ({source}), this terminal is {current}\n  -> open a {bound} terminal here: hydra shell {bound}\n  -> or run it once anyway: hydra allow -- {cmd}",
            bound = b.env,
            source = b.describe(),
        ));
    }
    if check.carries_owner()
        && !f.owners.is_empty()
        && let Some(owner) = f.target_owner
        && !f.owners.iter().any(|o| o.eq_ignore_ascii_case(owner))
    {
        let text = format!(
            "{owner} isn't in {current}'s github owners [{}]",
            f.owners.join(", ")
        );
        return if f.strict {
            Verdict::Block(format!("hydra: blocked {cmd} - {text}"))
        } else {
            Verdict::Warn(format!("hydra: warning: {text}"))
        };
    }
    Verdict::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Source};

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn git_classification() {
        assert_eq!(
            classify_git(&v("push origin main")).unwrap().label(),
            "git push"
        );
        assert_eq!(
            classify_git(&v("-C ../r -c a=b commit -m x")).unwrap(),
            Check::Git {
                action: GitAction::Commit,
                dir: Some("../r".into()),
                remote: None
            }
        );
        assert!(classify_git(&v("status")).is_none());
        assert!(classify_git(&v("log --oneline")).is_none());
    }

    #[test]
    fn gh_classification() {
        assert_eq!(
            classify_gh(&v("pr create --fill")).unwrap().label(),
            "gh pr create"
        );
        assert_eq!(
            classify_gh(&v("pr create -R acme/app")).unwrap(),
            Check::Gh {
                label: "gh pr create".into(),
                repo: Some("acme/app".into()),
                checks_owner: true
            }
        );
        assert!(classify_gh(&v("pr list")).is_none());
        assert!(classify_gh(&v("api user")).is_none());
        assert_eq!(
            classify_gh(&v("api repos/a/b/issues -f title=x"))
                .unwrap()
                .label(),
            "gh api"
        );
        assert_eq!(
            classify_gh(&v("api -X DELETE repos/a/b")).unwrap().label(),
            "gh api"
        );
        assert!(classify_gh(&v("api --method get repos/a/b")).is_none());
    }

    #[test]
    fn finds_git_in_chained_commands() {
        let c = checks_in_command_line(
            r#"cd ../repo && FOO=1 git -C "my dir" push; echo done | gh pr create -R a/b"#,
        );
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0],
            Check::Git {
                action: GitAction::Push,
                dir: Some("my dir".into()),
                remote: None
            }
        );
        assert_eq!(c[1].label(), "gh pr create");
        let c = checks_in_command_line("cd ../repo && git.exe commit -m 'a && b'");
        assert_eq!(
            c,
            vec![Check::Git {
                action: GitAction::Commit,
                dir: Some("../repo".into()),
                remote: None
            }]
        );
        assert!(checks_in_command_line("echo 'git push'").is_empty());
    }

    #[test]
    fn owners_from_urls() {
        assert_eq!(
            github_owner("https://github.com/acme/app.git").as_deref(),
            Some("acme")
        );
        assert_eq!(
            github_owner("git@github.com:me/x.git").as_deref(),
            Some("me")
        );
        assert_eq!(
            github_owner("ssh://git@github.com/me/x").as_deref(),
            Some("me")
        );
        assert_eq!(github_owner("https://gitlab.com/acme/app"), None);
    }

    fn bound(env: &str) -> Binding {
        Binding {
            env: env.into(),
            source: Source::Rule("E:/work/**".into()),
        }
    }
    fn facts<'a>(
        cur: Option<&'a str>,
        b: Option<&'a Binding>,
        owners: &'a [String],
        strict: bool,
        owner: Option<&'a str>,
    ) -> Facts<'a> {
        Facts {
            current_env: cur,
            allow: false,
            binding: b,
            owners,
            strict,
            target_owner: owner,
        }
    }
    fn push() -> Check {
        Check::Git {
            action: GitAction::Push,
            dir: None,
            remote: None,
        }
    }

    #[test]
    fn decisions() {
        let b = bound("work");
        assert_eq!(
            decide(&push(), &facts(None, Some(&b), &[], false, None)),
            Verdict::Allow
        );
        assert_eq!(
            decide(&push(), &facts(Some("work"), Some(&b), &[], false, None)),
            Verdict::Allow
        );
        let Verdict::Block(m) = decide(
            &push(),
            &facts(Some("personal"), Some(&b), &[], false, None),
        ) else {
            panic!()
        };
        assert!(m.starts_with("hydra: blocked git push - this folder belongs to work (rule E:/work/**), this terminal is personal"), "{m}");
        assert!(m.contains("-> open a work terminal here: hydra shell work"));
        assert!(m.contains("-> or run it once anyway: hydra allow -- git push"));
        let owners = vec!["acme".to_string()];
        assert!(matches!(
            decide(
                &push(),
                &facts(Some("work"), None, &owners, false, Some("me"))
            ),
            Verdict::Warn(_)
        ));
        assert!(matches!(
            decide(
                &push(),
                &facts(Some("work"), None, &owners, true, Some("me"))
            ),
            Verdict::Block(_)
        ));
        assert_eq!(
            decide(
                &push(),
                &facts(Some("work"), None, &owners, true, Some("ACME"))
            ),
            Verdict::Allow
        );
        let mut f = facts(Some("personal"), Some(&b), &[], false, None);
        f.allow = true;
        assert_eq!(decide(&push(), &f), Verdict::Allow);
    }
}
