//! Comment-preserving edits to `env.toml` that turn a tool on or off (`hydra add` / `hydra remove`).
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value, value};

/// Tools `hydra add` knows, in display order.
pub const TOOLS: &[&str] = &[
    "claude", "github", "git", "aws", "azure", "gcloud", "gws", "kube", "codex", "gemini",
];

/// Normalises a user-typed tool name: "gh" -> "github". Err(message listing TOOLS) if unknown.
pub fn tool_name(input: &str) -> Result<&'static str, String> {
    let lower = input.trim().to_ascii_lowercase();
    let key = if lower == "gh" { "github" } else { &lower };
    TOOLS
        .iter()
        .copied()
        .find(|t| *t == key)
        .ok_or_else(|| format!("unknown tool {input:?}; tools are: {}", TOOLS.join(", ")))
}

/// Values for one tool. Each field is set only if Some / non-empty.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ToolValues {
    pub mcp_exclude: Vec<String>,    // claude: mcp.exclude
    pub owners: Vec<String>,         // github: owners
    pub strict: Option<bool>,        // github: strict
    pub name: Option<String>,        // git: name
    pub email: Option<String>,       // git: email
    pub ssh_key: Option<String>,     // git: ssh_key
    pub signing_key: Option<String>, // git: signing_key
    pub profile: Option<String>,     // aws: profile
    pub isolate: Option<bool>,       // aws: isolate
    pub credentials: Option<String>, // gws: credentials ("secret:..." value)
    pub kube_config: Option<String>, // kube: config
    pub api_key: Option<String>,     // gemini: api_key ("secret:..." value)
    pub from: Option<String>,        // any borrowable tool: from (owner env)
}

#[derive(Debug, PartialEq)]
pub enum AddOutcome {
    Added,
    Updated,
    AlreadyPresent,
}

fn parse(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>().map_err(|e| e.to_string())
}

fn has_table(doc: &DocumentMut, tool: &str) -> bool {
    doc.get(tool).is_some_and(|i| i.as_table_like().is_some())
}

/// Rejects values that don't belong to `tool`.
fn check_applies(tool: &str, v: &ToolValues) -> Result<(), String> {
    if v.from.is_some() {
        if crate::borrow::NOT_BORROWABLE.contains(&tool) {
            return Err(format!(
                "{tool} can't be borrowed: commit author and SSH key stay per environment"
            ));
        }
        // [github] owners and strict are guard policy, which stays per environment.
        let own = tool == "github";
        let only_from = ToolValues {
            from: v.from.clone(),
            owners: if own { v.owners.clone() } else { Vec::new() },
            strict: if own { v.strict } else { None },
            ..Default::default()
        };
        if *v != only_from {
            return Err(
                "--from can't be combined with other settings; the owner's are used".to_string(),
            );
        }
    }
    let given = [
        (!v.mcp_exclude.is_empty(), "--mcp-exclude", "claude"),
        (!v.owners.is_empty(), "--owner", "github"),
        (v.strict.is_some(), "--strict", "github"),
        (v.name.is_some(), "--name", "git"),
        (v.email.is_some(), "--email", "git"),
        (v.ssh_key.is_some(), "--ssh-key", "git"),
        (v.signing_key.is_some(), "--signing-key", "git"),
        (v.profile.is_some(), "--profile", "aws"),
        (v.isolate.is_some(), "--isolate", "aws"),
        (v.credentials.is_some(), "--credentials-secret", "gws"),
        (v.kube_config.is_some(), "--kubeconfig", "kube"),
        (v.api_key.is_some(), "--api-key-secret", "gemini"),
    ];
    for (present, flag, owner) in given {
        if present && owner != tool {
            return Err(format!("{flag} only applies to {owner}"));
        }
    }
    Ok(())
}

/// Sets a key, keeping its comments; does nothing if the value is already the same.
fn set_value(t: &mut dyn TableLike, key: &str, new: Value) {
    match t.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            if old.to_string().trim() == new.to_string().trim() {
                return;
            }
            let mut n = new;
            *n.decor_mut() = old.decor().clone();
            *old = n;
        }
        None => {
            t.insert(key, Item::Value(new));
        }
    }
}

fn array_of(items: &[String]) -> Value {
    let mut a = Array::new();
    for s in items {
        a.push(s.as_str());
    }
    Value::Array(a)
}

fn has_key(t: &dyn TableLike, key: &str) -> bool {
    t.get(key).is_some_and(|i| !i.is_none())
}

fn fill(t: &mut dyn TableLike, tool: &str, v: &ToolValues) -> Result<(), String> {
    let strings: &[(&str, &Option<String>)] = match tool {
        "git" => &[
            ("name", &v.name),
            ("email", &v.email),
            ("ssh_key", &v.ssh_key),
            ("signing_key", &v.signing_key),
        ],
        "aws" => &[("profile", &v.profile)],
        "gws" => &[("credentials", &v.credentials)],
        "kube" => &[("config", &v.kube_config)],
        "gemini" => &[("api_key", &v.api_key)],
        _ => &[],
    };
    for (key, val) in strings {
        if let Some(s) = val {
            set_value(t, key, Value::from(s.as_str()));
        }
    }
    match tool {
        "claude" if !v.mcp_exclude.is_empty() => {
            let arr = array_of(&v.mcp_exclude);
            if let Some(mcp) = t.get_mut("mcp").and_then(Item::as_table_like_mut) {
                set_value(mcp, "exclude", arr);
            } else {
                let mut mcp = Table::new();
                mcp.set_dotted(true);
                mcp.insert("exclude", value(arr));
                t.insert("mcp", Item::Table(mcp));
            }
        }
        "github" => {
            if !v.owners.is_empty() {
                set_value(t, "owners", array_of(&v.owners));
            }
            if let Some(s) = v.strict {
                set_value(t, "strict", Value::from(s));
            }
        }
        "aws" => {
            if let Some(i) = v.isolate {
                set_value(t, "isolate", Value::from(i));
            }
            if !has_key(t, "profile") {
                return Err("aws needs --profile".to_string());
            }
        }
        "gemini" if !has_key(t, "api_key") => {
            return Err("gemini needs --api-key-secret".to_string());
        }
        _ => {}
    }
    Ok(())
}

/// Adds (or updates) the tool's table. Returns the new text.
pub fn add_tool(text: &str, tool: &str, v: &ToolValues) -> Result<(String, AddOutcome), String> {
    let tool = tool_name(tool)?;
    check_applies(tool, v)?;
    let mut doc = parse(text)?;
    if let Some(owner) = &v.from {
        let mut table = Table::new();
        table.insert("from", value(owner.as_str()));
        let existed = has_table(&doc, tool);
        if !existed && doc.contains_key(tool) {
            return Err(format!("{tool} in env.toml isn't a table"));
        }
        // The keys a borrowed section keeps (github owners/strict): the old values, then the flags.
        if let Some(old) = doc.get(tool).and_then(Item::as_table_like) {
            for key in crate::borrow::own_keys(tool) {
                if let Some(Item::Value(val)) = old.get(key) {
                    table.insert(key, Item::Value(val.clone()));
                }
            }
        }
        if tool == "github" {
            fill(&mut table, tool, v)?;
        }
        if existed {
            let old = doc
                .get(tool)
                .and_then(|i| i.as_table())
                .map(|t| t.decor().clone());
            if let Some(d) = old {
                *table.decor_mut() = d;
            }
        } else if !text.is_empty() && !text.ends_with("\n\n") {
            let prefix = if text.ends_with('\n') { "\n" } else { "\n\n" };
            table.decor_mut().set_prefix(prefix);
        }
        doc.insert(tool, Item::Table(table));
        let out = doc.to_string();
        let outcome = match (existed, out == text) {
            (true, true) => AddOutcome::AlreadyPresent,
            (true, false) => AddOutcome::Updated,
            (false, _) => AddOutcome::Added,
        };
        return Ok((out, outcome));
    }
    if has_table(&doc, tool) {
        let t = doc
            .get_mut(tool)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{tool} in env.toml isn't a table"))?;
        fill(t, tool, v)?;
        let out = doc.to_string();
        let outcome = if out == text {
            AddOutcome::AlreadyPresent
        } else {
            AddOutcome::Updated
        };
        return Ok((out, outcome));
    }
    if doc.contains_key(tool) {
        return Err(format!("{tool} in env.toml isn't a table"));
    }
    let mut table = Table::new();
    fill(&mut table, tool, v)?;
    if !text.is_empty() && !text.ends_with("\n\n") {
        let prefix = if text.ends_with('\n') { "\n" } else { "\n\n" };
        table.decor_mut().set_prefix(prefix);
    }
    doc.insert(tool, Item::Table(table));
    Ok((doc.to_string(), AddOutcome::Added))
}

/// Removes the tool's table. Ok((text, false)) if it wasn't there.
pub fn remove_tool(text: &str, tool: &str) -> Result<(String, bool), String> {
    let tool = tool_name(tool)?;
    let mut doc = parse(text)?;
    if !has_table(&doc, tool) {
        return Ok((text.to_string(), false));
    }
    doc.remove(tool);
    Ok((doc.to_string(), true))
}

/// Which of TOOLS have a table in this env.toml text.
pub fn tools_in(text: &str) -> Result<Vec<&'static str>, String> {
    let doc = parse(text)?;
    Ok(TOOLS
        .iter()
        .copied()
        .filter(|t| has_table(&doc, t))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EnvConfig;
    use crate::envs::template;
    use crate::name::EnvName;

    fn tpl() -> String {
        template(&EnvName::parse("work").unwrap(), "#1f9a8a")
    }

    fn vals() -> ToolValues {
        ToolValues::default()
    }

    #[test]
    fn add_from_writes_only_from_and_replaces_settings() {
        let v = ToolValues {
            from: Some("personal".into()),
            ..Default::default()
        };
        let (out, outcome) = add_tool("", "claude", &v).unwrap();
        assert_eq!(outcome, AddOutcome::Added);
        assert_eq!(out, "[claude]\nfrom = \"personal\"\n");
        let (out, outcome) = add_tool("[claude]\nmcp.exclude = [\"x\"]\n", "claude", &v).unwrap();
        assert_eq!(outcome, AddOutcome::Updated);
        assert_eq!(out, "[claude]\nfrom = \"personal\"\n");
        let (_, outcome) = add_tool(&out, "claude", &v).unwrap();
        assert_eq!(outcome, AddOutcome::AlreadyPresent);
    }

    #[test]
    fn add_from_refuses_git_and_other_flags() {
        let git = ToolValues {
            from: Some("personal".into()),
            ..Default::default()
        };
        assert_eq!(
            add_tool("", "git", &git).unwrap_err(),
            "git can't be borrowed: commit author and SSH key stay per environment"
        );
        let mixed = ToolValues {
            from: Some("personal".into()),
            mcp_exclude: vec!["x".into()],
            ..Default::default()
        };
        assert_eq!(
            add_tool("", "claude", &mixed).unwrap_err(),
            "--from can't be combined with other settings; the owner's are used"
        );
        let mixed = ToolValues {
            from: Some("personal".into()),
            owners: vec!["me".into()],
            ..Default::default()
        };
        assert_eq!(
            add_tool("", "claude", &mixed).unwrap_err(),
            "--from can't be combined with other settings; the owner's are used"
        );
    }

    #[test]
    fn add_from_github_keeps_its_own_owners_and_strict() {
        let v = ToolValues {
            from: Some("personal".into()),
            owners: vec!["acme".into()],
            strict: Some(true),
            ..Default::default()
        };
        let (out, outcome) = add_tool("", "github", &v).unwrap();
        assert_eq!(outcome, AddOutcome::Added);
        assert_eq!(
            out,
            "[github]\nfrom = \"personal\"\nowners = [\"acme\"]\nstrict = true\n"
        );
        // Turning an owned [github] into a borrowed one keeps its guard policy.
        let only_from = ToolValues {
            from: Some("personal".into()),
            ..Default::default()
        };
        let (out, outcome) = add_tool(
            "[github]\nowners = [\"acme\"]\nstrict = true\n",
            "github",
            &only_from,
        )
        .unwrap();
        assert_eq!(outcome, AddOutcome::Updated);
        assert_eq!(
            out,
            "[github]\nfrom = \"personal\"\nowners = [\"acme\"]\nstrict = true\n"
        );
    }

    #[test]
    fn adds_claude_to_template_keeping_comments() {
        // Start from the template minus its [claude] header so the add really creates the table.
        let src: String = tpl()
            .lines()
            .filter(|l| !l.starts_with("[claude]"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(!src.contains("[claude]"));
        let (out, o) = add_tool(&src, "claude", &vals()).unwrap();
        assert_eq!(o, AddOutcome::Added);
        assert!(out.contains("\n[claude]\n"), "{out}");
        for line in src.lines().filter(|l| l.trim_start().starts_with('#')) {
            assert!(out.contains(line), "lost {line:?}\n{out}");
        }
        let cfg: EnvConfig = toml::from_str(&out).unwrap();
        assert!(cfg.claude.is_some());
    }

    #[test]
    fn adding_claude_twice_is_already_present() {
        let src = "label = \"x\"\n";
        let (once, o) = add_tool(src, "claude", &vals()).unwrap();
        assert_eq!(o, AddOutcome::Added);
        let (twice, o) = add_tool(&once, "claude", &vals()).unwrap();
        assert_eq!(o, AddOutcome::AlreadyPresent);
        assert_eq!(once, twice);
        // The template starts with no tools: adding claude to it is a real addition.
        let (_, o) = add_tool(&tpl(), "claude", &vals()).unwrap();
        assert_eq!(o, AddOutcome::Added);
    }

    #[test]
    fn claude_mcp_exclude_round_trips() {
        let v = ToolValues {
            mcp_exclude: vec!["a-*".into(), "b".into()],
            ..vals()
        };
        let (out, o) = add_tool("label = \"x\"\n", "claude", &v).unwrap();
        assert_eq!(o, AddOutcome::Added);
        let cfg: EnvConfig = toml::from_str(&out).unwrap();
        assert_eq!(cfg.claude.unwrap().mcp.exclude, vec!["a-*", "b"]);
    }

    #[test]
    fn mcp_exclude_updates_existing_claude_table() {
        let v = ToolValues {
            mcp_exclude: vec!["z".into()],
            ..vals()
        };
        let (with_claude, _) = add_tool(&tpl(), "claude", &vals()).unwrap();
        let (out, o) = add_tool(&with_claude, "claude", &v).unwrap();
        assert_eq!(o, AddOutcome::Updated);
        let cfg: EnvConfig = toml::from_str(&out).unwrap();
        assert_eq!(cfg.claude.unwrap().mcp.exclude, vec!["z"]);
    }

    #[test]
    fn git_with_name_email_and_ssh_key() {
        let v = ToolValues {
            name: Some("A".into()),
            email: Some("a@b.c".into()),
            ssh_key: Some("~/.ssh/k".into()),
            ..vals()
        };
        let (out, o) = add_tool(&tpl(), "git", &v).unwrap();
        assert_eq!(o, AddOutcome::Added);
        let g = toml::from_str::<EnvConfig>(&out).unwrap().git.unwrap();
        assert_eq!(g.name.as_deref(), Some("A"));
        assert_eq!(g.email.as_deref(), Some("a@b.c"));
        assert_eq!(g.ssh_key.as_deref(), Some("~/.ssh/k"));
        assert_eq!(g.signing_key, None);
    }

    #[test]
    fn aws_needs_a_profile() {
        assert_eq!(
            add_tool(&tpl(), "aws", &vals()).unwrap_err(),
            "aws needs --profile"
        );
        let v = ToolValues {
            profile: Some("p".into()),
            isolate: Some(true),
            ..vals()
        };
        let (out, _) = add_tool(&tpl(), "aws", &v).unwrap();
        let a = toml::from_str::<EnvConfig>(&out).unwrap().aws.unwrap();
        assert_eq!((a.profile.as_str(), a.isolate), ("p", true));
        // An existing profile satisfies the requirement.
        let (_, o) = add_tool(&out, "aws", &vals()).unwrap();
        assert_eq!(o, AddOutcome::AlreadyPresent);
    }

    #[test]
    fn rejects_values_for_the_wrong_tool() {
        let v = ToolValues {
            profile: Some("p".into()),
            ..vals()
        };
        assert_eq!(
            add_tool(&tpl(), "claude", &v).unwrap_err(),
            "--profile only applies to aws"
        );
    }

    #[test]
    fn gemini_api_key_validates() {
        let v = ToolValues {
            api_key: Some("secret:work/gemini".into()),
            ..vals()
        };
        let (out, _) = add_tool(&tpl(), "gemini", &v).unwrap();
        let cfg: EnvConfig = toml::from_str(&out).unwrap();
        cfg.validate().unwrap();
        assert!(add_tool(&tpl(), "gemini", &vals()).is_err());
    }

    #[test]
    fn updates_only_the_given_keys() {
        let src = "[git]\nname = \"A\"\nemail = \"old@x.y\" # mine\n";
        let v = ToolValues {
            email: Some("new@x.y".into()),
            ..vals()
        };
        let (out, o) = add_tool(src, "git", &v).unwrap();
        assert_eq!(o, AddOutcome::Updated);
        assert_eq!(out, "[git]\nname = \"A\"\nemail = \"new@x.y\" # mine\n");
    }

    #[test]
    fn removes_a_tool_and_keeps_the_rest() {
        let src = "# top\nlabel = \"x\"\n\n[git]\nname = \"A\"\n\n# about aws\n[aws]\nprofile = \"p\" # c\n";
        let (out, removed) = remove_tool(src, "git").unwrap();
        assert!(removed);
        assert_eq!(
            out,
            "# top\nlabel = \"x\"\n\n# about aws\n[aws]\nprofile = \"p\" # c\n"
        );
        let (same, removed) = remove_tool(&out, "git").unwrap();
        assert!(!removed);
        assert_eq!(same, out);
    }

    #[test]
    fn tools_in_lists_present_tables_and_gh_resolves() {
        let src = "[aws]\nprofile = \"p\"\n[git]\n[env]\nA = \"b\"\n";
        assert_eq!(tools_in(src).unwrap(), vec!["git", "aws"]);
        assert_eq!(tool_name("gh").unwrap(), "github");
        assert_eq!(tool_name("GH").unwrap(), "github");
        let e = tool_name("nosuch").unwrap_err();
        assert!(e.contains("claude") && e.contains("gemini"), "{e}");
    }

    #[test]
    fn reports_parse_errors() {
        assert!(add_tool("[git\n", "git", &vals()).is_err());
        assert!(remove_tool("[git\n", "git").is_err());
        assert!(tools_in("[git\n").is_err());
    }
}
