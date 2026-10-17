//! The environment's accounts, as shown when its name in the status bar is clicked: one row
//! per provider check, info rows included; and the accounts that can be added there.
use hydra_core::provider::{IdentityReport, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub mark: &'static str,
    pub status: Status,
    pub provider: String,
    pub detail: String,
}

pub fn rows(reports: &[IdentityReport]) -> Vec<Row> {
    reports
        .iter()
        .map(|r| Row {
            mark: match r.status {
                Status::Ok => "✓",
                Status::Mismatch => "!",
                Status::Missing => "✗",
                Status::Info => "·",
            },
            status: r.status,
            provider: r.provider.clone(),
            detail: r.detail.clone(),
        })
        .collect()
}

/// The tools added with nothing but a sign-in: no values to ask for first.
pub const SIGN_IN_TOOLS: &[&str] = &["claude", "github", "azure", "gcloud", "codex"];

/// An account to add to an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddChoice {
    /// Turn the tool on, then sign in to it.
    SignIn(String),
    /// Use another environment's login for the tool.
    Borrow { tool: String, from: String },
}

impl AddChoice {
    pub fn label(&self) -> String {
        match self {
            AddChoice::SignIn(tool) => format!("Sign in to {tool}"),
            AddChoice::Borrow { tool, from } => format!("Use {tool} from {from}"),
        }
    }
}

/// What can be added to an environment whose tools are `on` (its own and borrowed): a sign-in
/// for each sign-in tool not on, and each tool another environment has on with its own login
/// (`others`: each environment with the tools it owns).
pub fn add_choices(on: &[String], others: &[(String, Vec<String>)]) -> Vec<AddChoice> {
    let off = |tool: &str| !on.iter().any(|t| t == tool);
    let sign_ins = SIGN_IN_TOOLS
        .iter()
        .filter(|t| off(t))
        .map(|t| AddChoice::SignIn(t.to_string()));
    let borrows = others.iter().flat_map(|(env, owned)| {
        owned.iter().filter(|t| off(t)).map(|t| AddChoice::Borrow {
            tool: t.clone(),
            from: env.clone(),
        })
    });
    sign_ins.chain(borrows).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sign_ins_are_offered_for_the_tools_not_on() {
        let got = add_choices(&strings(&["github", "git"]), &[]);
        assert_eq!(
            got,
            vec![
                AddChoice::SignIn("claude".into()),
                AddChoice::SignIn("azure".into()),
                AddChoice::SignIn("gcloud".into()),
                AddChoice::SignIn("codex".into()),
            ]
        );
        assert_eq!(got[0].label(), "Sign in to claude");
    }

    #[test]
    fn borrowing_is_offered_for_tools_other_envs_own() {
        let others = vec![
            ("personal".to_string(), strings(&["github", "aws"])),
            ("work".to_string(), strings(&["github"])),
        ];
        let got = add_choices(&strings(&["aws"]), &others);
        let borrows: Vec<String> = got
            .iter()
            .filter(|c| matches!(c, AddChoice::Borrow { .. }))
            .map(AddChoice::label)
            .collect();
        assert_eq!(
            borrows,
            vec!["Use github from personal", "Use github from work"]
        );
    }

    #[test]
    fn nothing_to_add_when_every_tool_is_on() {
        let on = strings(SIGN_IN_TOOLS);
        let others = vec![("personal".to_string(), strings(&["github"]))];
        assert!(add_choices(&on, &others).is_empty());
    }

    fn r(provider: &str, status: Status, detail: &str) -> IdentityReport {
        IdentityReport {
            provider: provider.into(),
            status,
            detail: detail.into(),
        }
    }

    #[test]
    fn every_check_is_a_row_marked_by_its_status() {
        let got = rows(&[
            r(
                "aws",
                Status::Ok,
                "arn:aws:sts::123456789012:assumed-role/dev/me",
            ),
            r("git", Status::Mismatch, "uses a@x, expected b@y"),
            r("claude", Status::Missing, "not signed in"),
            r("env", Status::Info, "TOKEN found"),
        ]);
        let marks: Vec<(&str, &str)> = got.iter().map(|r| (r.mark, r.provider.as_str())).collect();
        assert_eq!(
            marks,
            vec![("✓", "aws"), ("!", "git"), ("✗", "claude"), ("·", "env")]
        );
        assert_eq!(
            got[0].detail,
            "arn:aws:sts::123456789012:assumed-role/dev/me"
        );
        assert_eq!(got[1].status, Status::Mismatch);
    }

    #[test]
    fn no_checks_no_rows() {
        assert!(rows(&[]).is_empty());
    }
}
