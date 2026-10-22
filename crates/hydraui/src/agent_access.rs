//! What an environment's agents may know about what they can use (`hydra_access`): the tools
//! the environment has on and the variables its `[agents]` table tells them about, each with
//! what it is for. Names only: a value never leaves the terminal's own environment.
use hydra_core::config::load_env_lenient;
use hydra_core::name::EnvName;
use hydra_core::paths::HydraPaths;
use serde_json::{Value, json};

/// What a variable that hydra itself sets is, offered as the starting text when the user
/// tells agents about it.
pub fn default_about(var: &str) -> Option<&'static str> {
    Some(match var {
        "JEV_API_KEY" => {
            "TypeSafe's Jev decision model, for a judgement call about some text: ask it a \
             structured question (a choice, or a 0..1 score). POST \
             https://api.typesafe.ai/v1/systemone with `Authorization: Bearer $JEV_API_KEY` \
             and model \"jev-latest\"; docs at https://docs.typesafe.ai/api."
        }
        "GEMINI_API_KEY" => "A Google Gemini API key, for calling the Gemini API.",
        _ => return None,
    })
}

/// The `hydra_access` reply for a terminal in `env`.
pub fn describe(paths: &HydraPaths, env: &str) -> Result<Value, String> {
    let name = EnvName::parse(env).map_err(|e| e.to_string())?;
    let (cfg, failed) = load_env_lenient(paths, &name).map_err(|e| e.to_string())?;
    let tools: Vec<Value> = hydra_providers::all()
        .iter()
        .filter(|p| p.id() != "env" && p.is_configured(&cfg))
        .map(|p| {
            let mut t = json!({ "tool": p.id() });
            if let Some(owner) = cfg.borrowed.get(p.id()) {
                t["borrowed_from"] = json!(owner.as_str());
            }
            t
        })
        .collect();
    let credentials: Vec<Value> = cfg
        .agents
        .iter()
        .map(|(var, about)| json!({ "variable": var, "about": about }))
        .collect();
    let mut doc = json!({
        "environment": env,
        "tools": tools,
        "credentials": credentials,
        "note": "Each credential is an environment variable already set in this terminal's \
                 shell (and so in commands you run). Use it from there; hydra never sends a \
                 value through this tool.",
    });
    if let Some(label) = &cfg.label {
        doc["label"] = json!(label);
    }
    if !failed.is_empty() {
        doc["problems"] = json!(
            failed
                .iter()
                .map(|(tool, e)| format!("{tool}: {e}"))
                .collect::<Vec<_>>()
        );
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(envs: &[(&str, &str)]) -> (tempfile::TempDir, HydraPaths) {
        let d = tempfile::tempdir().unwrap();
        let paths = HydraPaths::new(d.path());
        for (name, text) in envs {
            let file = paths.env_file(&EnvName::parse(name).unwrap());
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        (d, paths)
    }

    #[test]
    fn tools_and_the_variables_agents_are_told_about() {
        let (_d, paths) = home(&[
            ("personal", "[codex]\n"),
            (
                "work",
                "label = \"Work\"\n[jev]\napi_key = \"secret:work/jev\"\n[codex]\nfrom = \"personal\"\n\
                 [env]\nLINEAR_API_KEY = \"secret:work/linear\"\n[agents]\nJEV_API_KEY = \"Jev\"\n",
            ),
        ]);
        let doc = describe(&paths, "work").unwrap();
        assert_eq!(doc["environment"], "work");
        assert_eq!(doc["label"], "Work");
        let tools = doc["tools"].as_array().unwrap();
        assert!(tools.contains(&json!({"tool": "jev"})), "{doc}");
        assert!(
            tools.contains(&json!({"tool": "codex", "borrowed_from": "personal"})),
            "{doc}"
        );
        assert_eq!(
            doc["credentials"],
            json!([{"variable": "JEV_API_KEY", "about": "Jev"}])
        );
        let text = doc.to_string();
        assert!(
            !text.contains("LINEAR"),
            "untold variables stay out: {text}"
        );
        assert!(!text.contains("secret:"), "{text}");
    }

    #[test]
    fn an_unknown_environment_is_an_error() {
        let (_d, paths) = home(&[]);
        assert!(describe(&paths, "nope").is_err());
    }

    #[test]
    fn hydras_own_variables_have_a_starting_description() {
        assert!(default_about("JEV_API_KEY").unwrap().contains("Jev"));
        assert_eq!(default_about("REGION"), None);
    }
}
