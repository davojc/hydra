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
        for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            if entry.path().join("SKILL.md").is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                names.insert(name.to_string());
            }
        }
    }
    names.into_iter().collect()
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
}
