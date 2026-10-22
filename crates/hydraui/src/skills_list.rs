//! The skills a stage can name in Pipeline settings (orchestrator spec §7.4): those installed
//! for the environment's Claude and in the project, by folder name.

use std::collections::BTreeSet;
use std::path::Path;

/// The skills in `<claude_config>/skills` and `<project>/.claude/skills`: each folder holding a
/// `SKILL.md`, by its name, once, sorted.
pub fn installed(claude_config: Option<&Path>, project: &Path) -> Vec<String> {
    let project_skills = project.join(".claude").join("skills");
    let roots = claude_config
        .map(|c| c.join("skills"))
        .into_iter()
        .chain(std::iter::once(project_skills));
    let mut names = BTreeSet::new();
    for root in roots {
        // claude.ai's synced skills sit one folder further down: skills/synced/<org>/<name>.
        let synced = std::fs::read_dir(root.join("synced"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path());
        for dir in std::iter::once(root.clone()).chain(synced) {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if entry.path().join("SKILL.md").is_file()
                    && let Some(name) = entry.file_name().to_str()
                {
                    names.insert(name.to_string());
                }
            }
        }
    }
    names.into_iter().collect()
}

/// The skills plugins give, from `<claude_config>/plugins`: every `skills/<name>/SKILL.md` a
/// few folders down (marketplaces and the plugin cache).
fn plugin_skills(claude_config: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, depth: usize, out: &mut BTreeSet<String>) {
        if depth == 0 {
            return;
        }
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if entry.file_name() == "skills" {
                for skill in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                    if skill.path().join("SKILL.md").is_file()
                        && let Some(name) = skill.file_name().to_str()
                    {
                        out.insert(name.to_string());
                    }
                }
            } else {
                walk(&path, depth - 1, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&claude_config.join("plugins"), 7, &mut out);
    out
}

/// Every skill a session could use when started in one of `folders`: each folder's, the
/// environment's, claude.ai's synced ones and the plugins', once, sorted (the settings' skill
/// picker).
pub fn available(claude_config: Option<&Path>, folders: &[std::path::PathBuf]) -> Vec<String> {
    let mut names: BTreeSet<String> = folders
        .iter()
        .flat_map(|f| installed(claude_config, f))
        .collect();
    if let Some(c) = claude_config {
        names.extend(plugin_skills(c));
    }
    names.into_iter().collect()
}

/// Whether a session started in `folder` can use `skill` (orchestrator spec §4): it is among
/// the folder's and the environment's skills, claude.ai's synced ones, or a plugin's. A
/// `plugin:name` skill is looked up by its name; names ignore case.
pub fn is_installed(skill: &str, claude_config: Option<&Path>, folder: &Path) -> bool {
    let wanted = skill.rsplit(':').next().unwrap_or(skill).trim();
    let same = |n: &String| n.eq_ignore_ascii_case(wanted);
    installed(claude_config, folder).iter().any(same)
        || claude_config.is_some_and(|c| plugin_skills(c).iter().any(same))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_skills_are_listed_once_sorted() {
        let d = tempfile::tempdir().unwrap();
        let claude = d.path().join("claude");
        let project = d.path().join("project");
        for (root, name) in [
            (claude.join("skills"), "qa-skills"),
            (claude.join("skills"), "angular"),
            (project.join(".claude").join("skills"), "angular"),
            (project.join(".claude").join("skills"), "release-notes"),
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        }
        // A folder without SKILL.md isn't a skill.
        std::fs::create_dir_all(claude.join("skills").join("notes")).unwrap();
        assert_eq!(
            installed(Some(&claude), &project),
            ["angular", "qa-skills", "release-notes"]
        );
        assert!(installed(None, &d.path().join("nowhere")).is_empty());
    }

    #[test]
    fn available_lists_every_folders_skills_and_plugins_once() {
        let d = tempfile::tempdir().unwrap();
        let claude = d.path().join("claude");
        let mk = |dir: std::path::PathBuf| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "x").unwrap();
        };
        let qa = d.path().join("QA-Skills");
        let dev = d.path().join("iov42-agents");
        mk(qa.join(".claude/skills/qa"));
        mk(dev.join(".claude/skills/angular"));
        mk(claude.join("skills/rust"));
        mk(claude.join("plugins/cache/iov42/backend-dev/1.0.0/skills/be-develop"));
        assert_eq!(
            available(Some(&claude), &[qa.clone(), dev, qa]),
            ["angular", "be-develop", "qa", "rust"]
        );
    }

    #[test]
    fn a_skill_is_installed_from_the_folder_synced_skills_or_a_plugin() {
        let d = tempfile::tempdir().unwrap();
        let claude = d.path().join("claude");
        let folder = d.path().join("QA-Skills");
        let mk = |dir: std::path::PathBuf| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        };
        mk(folder.join(".claude").join("skills").join("qa"));
        mk(claude
            .join("skills")
            .join("synced")
            .join("org_1")
            .join("iov42-skills"));
        mk(claude
            .join("plugins")
            .join("cache")
            .join("iov42")
            .join("qa-engineer")
            .join("0.1.0")
            .join("skills")
            .join("qa-replicate-bug"));
        assert!(is_installed("qa", Some(&claude), &folder));
        assert!(
            is_installed("QA", Some(&claude), &folder),
            "names ignore case"
        );
        assert!(is_installed("iov42-skills", Some(&claude), &folder));
        assert!(is_installed("qa-replicate-bug", Some(&claude), &folder));
        assert!(is_installed(
            "qa-engineer:qa-replicate-bug",
            Some(&claude),
            &folder
        ));
        assert!(!is_installed("dev-pipeline", Some(&claude), &folder));
        assert!(!is_installed(
            "qa",
            Some(&claude),
            &d.path().join("elsewhere")
        ));
    }
}
