//! The environment's accounts, as shown when its name in the status bar is clicked: one row
//! per provider check, info rows included; and the accounts that can be added there.
use hydra_core::envedit::ToolValues;
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
pub const SIGN_IN_TOOLS: &[&str] = &["claude", "github", "azure", "gcloud", "codex", "gws"];

/// The tools added with a few values first (a form), then a sign-in if they have one.
pub const SETUP_TOOLS: &[&str] = &["git", "aws", "gemini", "kube"];

/// One field of a tool's set-up form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
    pub required: bool,
    pub masked: bool,
}

const fn field(
    key: &'static str,
    label: &'static str,
    placeholder: &'static str,
    required: bool,
) -> Field {
    Field {
        key,
        label,
        placeholder,
        required,
        masked: false,
    }
}

/// The form for setting up `tool` (one of `SETUP_TOOLS`).
pub fn form_fields(tool: &str) -> Vec<Field> {
    match tool {
        "git" => vec![
            field("name", "Name", "Your Name", true),
            field("email", "Email", "you@example.com", true),
            field("ssh_key", "SSH key (optional)", "~/.ssh/id_ed25519", false),
            field(
                "signing_key",
                "Signing key (optional)",
                "~/.ssh/id_ed25519.pub",
                false,
            ),
        ],
        "aws" => vec![field(
            "profile",
            "Profile",
            "the AWS profile to sign in with",
            true,
        )],
        "gemini" => vec![Field {
            masked: true,
            ..field("api_key", "API key", "stored as a hydra secret", true)
        }],
        "kube" => vec![field(
            "kubeconfig",
            "Kubeconfig (optional)",
            "empty: a kubeconfig of its own",
            false,
        )],
        _ => Vec::new(),
    }
}

/// What a filled-in form adds.
#[derive(Debug, Clone, PartialEq)]
pub enum FormResult {
    Values(Box<ToolValues>),
    /// Gemini's key, to store as a secret first.
    Gemini(String),
}

/// The values typed into `tool`'s form, trimmed; empty optional fields are left out. Says
/// which required field is empty.
pub fn form_values(tool: &str, typed: &[(&str, String)]) -> Result<FormResult, String> {
    let get = |key: &str| {
        typed
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    for f in form_fields(tool).iter().filter(|f| f.required) {
        if get(f.key).is_none() {
            return Err(format!("{} is needed", f.label));
        }
    }
    Ok(match tool {
        "gemini" => FormResult::Gemini(get("api_key").unwrap_or_default()),
        _ => FormResult::Values(Box::new(ToolValues {
            name: get("name"),
            email: get("email"),
            ssh_key: get("ssh_key"),
            signing_key: get("signing_key"),
            profile: get("profile"),
            kube_config: get("kubeconfig"),
            ..Default::default()
        })),
    })
}

/// An account to add to an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddChoice {
    /// Turn the tool on, then sign in to it.
    SignIn(String),
    /// Ask for the tool's values, turn it on, then sign in if it has a sign-in.
    SetUp(String),
    /// Use another environment's login for the tool.
    Borrow { tool: String, from: String },
}

impl AddChoice {
    pub fn label(&self) -> String {
        match self {
            AddChoice::SignIn(tool) => format!("Sign in to {tool}"),
            AddChoice::SetUp(tool) => format!("Set up {tool}…"),
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
    let set_ups = SETUP_TOOLS
        .iter()
        .filter(|t| off(t))
        .map(|t| AddChoice::SetUp(t.to_string()));
    let borrows = others.iter().flat_map(|(env, owned)| {
        owned.iter().filter(|t| off(t)).map(|t| AddChoice::Borrow {
            tool: t.clone(),
            from: env.clone(),
        })
    });
    sign_ins.chain(set_ups).chain(borrows).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn typed(pairs: &[(&str, &str)]) -> Vec<(&'static str, String)> {
        pairs
            .iter()
            .map(|(k, v)| {
                let key = form_fields("git")
                    .into_iter()
                    .chain(form_fields("aws"))
                    .chain(form_fields("gemini"))
                    .chain(form_fields("kube"))
                    .find(|f| f.key == *k)
                    .unwrap()
                    .key;
                (key, v.to_string())
            })
            .collect()
    }

    #[test]
    fn each_set_up_tool_has_its_fields() {
        let keys = |t: &str| form_fields(t).iter().map(|f| f.key).collect::<Vec<_>>();
        assert_eq!(keys("git"), vec!["name", "email", "ssh_key", "signing_key"]);
        assert_eq!(keys("aws"), vec!["profile"]);
        assert_eq!(keys("gemini"), vec!["api_key"]);
        assert_eq!(keys("kube"), vec!["kubeconfig"]);
        assert!(form_fields("gemini")[0].masked);
    }

    #[test]
    fn form_values_need_the_required_fields_and_drop_empty_ones() {
        let git = form_values(
            "git",
            &typed(&[
                ("name", " Dee "),
                ("email", "dee@example.com"),
                ("ssh_key", ""),
            ]),
        )
        .unwrap();
        let FormResult::Values(v) = git else { panic!() };
        assert_eq!(v.name.as_deref(), Some("Dee"));
        assert_eq!(v.email.as_deref(), Some("dee@example.com"));
        assert_eq!(v.ssh_key, None);
        assert!(
            form_values("git", &typed(&[("name", "Dee")]))
                .unwrap_err()
                .contains("Email")
        );
        assert!(form_values("aws", &typed(&[("profile", "  ")])).is_err());
        let FormResult::Values(k) = form_values("kube", &typed(&[("kubeconfig", "")])).unwrap()
        else {
            panic!()
        };
        assert_eq!(k.kube_config, None);
        assert_eq!(
            form_values("gemini", &typed(&[("api_key", "AIza")])).unwrap(),
            FormResult::Gemini("AIza".into())
        );
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
                AddChoice::SignIn("gws".into()),
                AddChoice::SetUp("aws".into()),
                AddChoice::SetUp("gemini".into()),
                AddChoice::SetUp("kube".into()),
            ]
        );
        assert_eq!(got[5].label(), "Set up aws…");
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
        let mut on = strings(SIGN_IN_TOOLS);
        on.extend(strings(SETUP_TOOLS));
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
