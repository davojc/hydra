//! Reading and writing project folders in a checkout of the projects repo (spec §2).

use std::path::{Path, PathBuf};

use hydraui_sets::layout::write_atomic;

use crate::Error;
use crate::front_matter;
use crate::model::*;

pub const PROJECT_FILE: &str = "project.toml";

/// Every project folder under `root` (a folder holding `project.toml`), by slug. A folder whose
/// files don't parse is returned as an error beside the others, so one bad project doesn't hide
/// the rest.
pub fn load_all(root: &Path) -> Vec<(String, Result<Project, Error>)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join(PROJECT_FILE).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    slugs.sort();
    slugs
        .into_iter()
        .map(|slug| {
            let project = load(&root.join(&slug));
            (slug, project)
        })
        .collect()
}

/// Reads one project folder.
pub fn load(dir: &Path) -> Result<Project, Error> {
    let slug = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let file: ProjectFile = read_toml(&dir.join(PROJECT_FILE))?;
    let spec = read_optional(&dir.join("spec.md"))?;
    let milestones = match read_optional(&dir.join("milestones.toml"))? {
        text if text.trim().is_empty() => Vec::new(),
        text => {
            toml::from_str::<MilestonesFile>(&text)
                .map_err(|e| Error::in_file(&dir.join("milestones.toml"), e))?
                .milestones
        }
    };
    let mut tickets = Vec::new();
    for (id, path) in markdown_files(&dir.join("tickets"))? {
        let (meta, body) = front_matter::parse(&std::fs::read_to_string(&path)?)
            .map_err(|e| e.with_file(&path))?;
        tickets.push(Ticket { id, meta, body });
    }
    sort_by_number(&mut tickets, |t| &t.id);
    let mut decisions = Vec::new();
    for (id, path) in markdown_files(&dir.join("decisions"))? {
        let (meta, body) = front_matter::parse(&std::fs::read_to_string(&path)?)
            .map_err(|e| e.with_file(&path))?;
        decisions.push(Decision { id, meta, body });
    }
    sort_by_number(&mut decisions, |d| &d.id);
    let mut meetings = Vec::new();
    for (name, path) in markdown_files(&dir.join("meetings"))? {
        meetings.push(Meeting {
            name,
            body: std::fs::read_to_string(&path)?,
        });
    }
    // Newest first: names start with the date.
    meetings.sort_by(|a, b| b.name.cmp(&a.name));
    let resources = dir.join("resources");
    let links = match read_optional(&resources.join("links.toml"))? {
        text if text.trim().is_empty() => Vec::new(),
        text => {
            toml::from_str::<LinksFile>(&text)
                .map_err(|e| Error::in_file(&resources.join("links.toml"), e))?
                .links
        }
    };
    let mut files: Vec<String> = match std::fs::read_dir(&resources) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n != "links.toml" && !n.starts_with('.'))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    let mut progress = Vec::new();
    if let Ok(people) = std::fs::read_dir(dir.join("progress")) {
        for person in people.flatten().filter(|e| e.path().is_dir()) {
            let Ok(login) = person.file_name().into_string() else {
                continue;
            };
            for (date, path) in markdown_files(&person.path())? {
                progress.push(Progress {
                    login: login.clone(),
                    date,
                    body: std::fs::read_to_string(&path)?,
                });
            }
        }
    }
    progress.sort_by(|a, b| b.date.cmp(&a.date).then(a.login.cmp(&b.login)));
    Ok(Project {
        slug,
        file,
        spec,
        milestones,
        tickets,
        decisions,
        meetings,
        files,
        links,
        progress,
    })
}

pub fn write_project_file(dir: &Path, file: &ProjectFile) -> Result<(), Error> {
    write_toml(&dir.join(PROJECT_FILE), file)
}

pub fn write_spec(dir: &Path, spec: &str) -> Result<(), Error> {
    Ok(write_atomic(&dir.join("spec.md"), spec)?)
}

pub fn write_milestones(dir: &Path, milestones: &[Milestone]) -> Result<(), Error> {
    write_toml(
        &dir.join("milestones.toml"),
        &MilestonesFile {
            milestones: milestones.to_vec(),
        },
    )
}

pub fn write_ticket(dir: &Path, ticket: &Ticket) -> Result<(), Error> {
    let path = dir.join("tickets").join(format!("{}.md", ticket.id));
    Ok(write_atomic(
        &path,
        &front_matter::render(&ticket.meta, &ticket.body)?,
    )?)
}

pub fn write_decision(dir: &Path, decision: &Decision) -> Result<(), Error> {
    let path = dir.join("decisions").join(format!("{}.md", decision.id));
    Ok(write_atomic(
        &path,
        &front_matter::render(&decision.meta, &decision.body)?,
    )?)
}

pub fn write_meeting(dir: &Path, meeting: &Meeting) -> Result<(), Error> {
    let path = dir.join("meetings").join(format!("{}.md", meeting.name));
    Ok(write_atomic(&path, &meeting.body)?)
}

pub fn write_links(dir: &Path, links: &[Link]) -> Result<(), Error> {
    write_toml(
        &dir.join("resources").join("links.toml"),
        &LinksFile {
            links: links.to_vec(),
        },
    )
}

/// Largest resource file a project takes (spec §2).
pub const MAX_RESOURCE_BYTES: u64 = 10 * 1024 * 1024;

/// Copies `from` into `resources/`, keeping its name (or adding ` (2)`, ` (3)` … when taken).
/// Returns the name it was saved under.
pub fn add_resource(dir: &Path, from: &Path) -> Result<String, Error> {
    add_resource_as(dir, from, "")
}

/// Copies `from` into `resources/` as `name` (its own name when empty; its extension added
/// when `name` has none), never over another file. Returns the name it was saved under.
pub fn add_resource_as(dir: &Path, from: &Path, name: &str) -> Result<String, Error> {
    let size = std::fs::metadata(from)?.len();
    if size > MAX_RESOURCE_BYTES {
        return Err(Error::TooLarge {
            name: from.display().to_string(),
            bytes: size,
        });
    }
    let own = from
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Invalid(format!("{} has no file name", from.display())))?;
    let name = resource_name(name, own)?;
    let resources = dir.join("resources");
    std::fs::create_dir_all(&resources)?;
    let name = free_name(&resources, &name);
    std::fs::copy(from, resources.join(&name))?;
    Ok(name)
}

/// Renames resource file `old` to `new` (its extension kept when `new` has none). Returns the
/// new name; refused if another file has it.
pub fn rename_resource(dir: &Path, old: &str, new: &str) -> Result<String, Error> {
    let resources = dir.join("resources");
    let from = resources.join(old);
    if old.contains(['/', '\\']) || !from.is_file() {
        return Err(Error::Invalid(format!("There's no file {old}.")));
    }
    let name = resource_name(new, old)?;
    if name == old {
        return Ok(name);
    }
    let to = resources.join(&name);
    // A case-only rename is the same file on macOS and Windows.
    if to.exists() && !name.eq_ignore_ascii_case(old) {
        return Err(Error::Invalid(format!(
            "There's already a file called {name}."
        )));
    }
    std::fs::rename(from, to)?;
    Ok(name)
}

/// Gives link `url` the title `title`.
pub fn rename_link(dir: &Path, url: &str, title: &str) -> Result<(), Error> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Invalid("Give the link a title.".into()));
    }
    let mut links = load(dir)?.links;
    let link = links
        .iter_mut()
        .find(|l| l.url == url)
        .ok_or_else(|| Error::Invalid(format!("There's no link {url}.")))?;
    link.title = title.to_string();
    write_links(dir, &links)
}

/// The file name to save as, from what the user typed (`wanted`) and the file's own name.
fn resource_name(wanted: &str, own: &str) -> Result<String, Error> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Ok(own.to_string());
    }
    if wanted.contains(['/', '\\', ':']) || wanted.starts_with('.') {
        return Err(Error::Invalid(
            "A name can't start with a dot or hold / \\ or :.".into(),
        ));
    }
    let ext = match own.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => Some(ext),
        _ => None,
    };
    let name = match ext {
        Some(ext)
            if !wanted
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", ext.to_ascii_lowercase())) =>
        {
            format!("{wanted}.{ext}")
        }
        _ => wanted.to_string(),
    };
    if name.eq_ignore_ascii_case("links.toml") {
        return Err(Error::Invalid("links.toml is taken by the links.".into()));
    }
    Ok(name)
}

pub fn write_progress(dir: &Path, progress: &Progress) -> Result<(), Error> {
    let path = dir
        .join("progress")
        .join(&progress.login)
        .join(format!("{}.md", progress.date));
    Ok(write_atomic(&path, &progress.body)?)
}

/// A folder name for a project called `name`: lowercase, words joined by `-`.
pub fn slug_for(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// A ticket key suggested from a project's name (spec §9.1): the initials of its words, or the
/// first two letters of a one-word name, upper case.
pub fn key_for(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let key: String = if words.len() > 1 {
        words
            .iter()
            .filter_map(|w| w.chars().next())
            .take(4)
            .collect()
    } else {
        words
            .first()
            .map(|w| w.chars().take(2).collect())
            .unwrap_or_default()
    };
    key.to_ascii_uppercase()
}

fn free_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !dir.join(candidate).exists())
        .unwrap_or_else(|| name.to_string())
}

fn markdown_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") || !path.is_file() {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            out.push((stem.to_string(), path));
        }
    }
    out.sort();
    Ok(out)
}

/// Sorts ids like `PG-2`, `PG-10` by their number, not as text.
fn sort_by_number<T>(items: &mut [T], id: impl Fn(&T) -> &str) {
    items.sort_by_key(|item| {
        let id = id(item);
        (crate::number_of(id).unwrap_or(u32::MAX), id.to_string())
    });
}

fn read_optional(path: &Path) -> Result<String, Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let text = std::fs::read_to_string(path)?;
    toml::from_str(&text).map_err(|e| Error::in_file(path, e))
}

fn write_toml<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let text = toml::to_string(value).map_err(|e| Error::Toml(e.to_string()))?;
    Ok(write_atomic(path, &text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) {
        std::fs::create_dir_all(dir.join("tickets")).unwrap();
        std::fs::create_dir_all(dir.join("decisions")).unwrap();
        std::fs::write(
            dir.join("project.toml"),
            r#"
name = "Product Groups"
key = "PG"
lead = "jmiller-iov42"
members = ["aza-iov42"]
owner_team = "platform"   # unknown, kept

[sync]
times = ["09:00"]
"#,
        )
        .unwrap();
        std::fs::write(dir.join("spec.md"), "# Product Groups\n").unwrap();
        std::fs::write(
            dir.join("milestones.toml"),
            r#"
[[milestone]]
id = "M1"
title = "Groups model"
target = 2026-11-20
status = "active"

[[milestone]]
id = "M2"
title = "Sharing"
"#,
        )
        .unwrap();
        for (n, status) in [(2, "done"), (10, "in progress"), (1, "todo")] {
            std::fs::write(
                dir.join("tickets").join(format!("PG-{n}.md")),
                format!(
                    "+++\ntitle = \"T{n}\"\nstatus = \"{status}\"\nmilestone = \"M1\"\n+++\n\nBody {n}\n"
                ),
            )
            .unwrap();
        }
        std::fs::write(
            dir.join("decisions").join("D-1.md"),
            "+++\nquestion = \"Postgres or Mongo?\"\nassigned_to = \"jmiller-iov42\"\nasked_at = 2026-11-03T10:00:00Z\n+++\n",
        )
        .unwrap();
    }

    #[test]
    fn loads_a_project_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("product-groups");
        sample(&dir);
        let p = load(&dir).unwrap();
        assert_eq!(p.slug, "product-groups");
        assert_eq!(p.file.key, "PG");
        assert_eq!(p.file.sync.times, vec!["09:00"]);
        assert_eq!(p.milestones.len(), 2);
        assert_eq!(p.milestones[0].target.unwrap().to_string(), "2026-11-20");
        assert_eq!(p.milestones[1].status, MilestoneStatus::Planned);
        let ids: Vec<_> = p.tickets.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["PG-1", "PG-2", "PG-10"], "sorted by number");
        assert_eq!(p.tickets[2].meta.status, TicketStatus::InProgress);
        assert!(p.decisions[0].is_open());
    }

    #[test]
    fn items_without_a_review_count_as_approved() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        let p = load(&dir).unwrap();
        assert_eq!(p.milestones[0].review, Review::Approved);
        assert_eq!(p.tickets[0].meta.review, Review::Approved);
        let mut t = p.tickets[0].clone();
        t.meta.review = Review::Rejected;
        t.meta.review_note = "out of scope".into();
        write_ticket(&dir, &t).unwrap();
        let text = std::fs::read_to_string(dir.join("tickets/PG-1.md")).unwrap();
        assert!(
            text.contains("review = \"rejected\"")
                && text.contains("review_note = \"out of scope\"")
        );
        let counts = crate::progress::milestone_counts(&load(&dir).unwrap(), |t| t.meta.status);
        assert_eq!(counts["M1"].total(), 2, "a rejected ticket doesn't count");
    }

    #[test]
    fn a_missing_optional_file_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bare");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.toml"),
            "name = \"Bare\"\nkey = \"BA\"\nlead = \"me\"\n",
        )
        .unwrap();
        let p = load(&dir).unwrap();
        assert!(p.spec.is_empty() && p.milestones.is_empty() && p.tickets.is_empty());
        assert_eq!(p.file.sync, Sync::default());
    }

    #[test]
    fn unknown_keys_survive_a_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        let p = load(&dir).unwrap();
        write_project_file(&dir, &p.file).unwrap();
        let again = load(&dir).unwrap();
        assert_eq!(
            again.file.extra.get("owner_team").unwrap().as_str(),
            Some("platform")
        );
    }

    #[test]
    fn a_ticket_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        let mut t = load(&dir).unwrap().tickets.remove(0);
        t.meta.status = TicketStatus::InReview;
        t.meta.assignee = "aza-iov42".into();
        write_ticket(&dir, &t).unwrap();
        let back = load(&dir).unwrap().tickets.remove(0);
        assert_eq!(back, t);
    }

    #[test]
    fn one_bad_project_does_not_hide_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        sample(&tmp.path().join("good"));
        std::fs::create_dir_all(tmp.path().join("bad")).unwrap();
        std::fs::write(tmp.path().join("bad/project.toml"), "name = ").unwrap();
        std::fs::create_dir_all(tmp.path().join("not-a-project")).unwrap();
        let all = load_all(tmp.path());
        let slugs: Vec<_> = all.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(slugs, ["bad", "good"]);
        assert!(all[0].1.is_err() && all[1].1.is_ok());
    }

    #[test]
    fn resources_keep_their_name_and_never_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        let src = tmp.path().join("flow.png");
        std::fs::write(&src, b"png").unwrap();
        assert_eq!(add_resource(&dir, &src).unwrap(), "flow.png");
        assert_eq!(add_resource(&dir, &src).unwrap(), "flow (2).png");
        write_links(
            &dir,
            &[Link {
                title: "Figma".into(),
                url: "https://figma.com/x".into(),
            }],
        )
        .unwrap();
        let p = load(&dir).unwrap();
        assert_eq!(p.files, ["flow (2).png", "flow.png"]);
        assert_eq!(p.links[0].title, "Figma");
    }

    #[test]
    fn a_resource_is_saved_and_renamed_under_a_chosen_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        let src = tmp.path().join("IMG_0042.png");
        std::fs::write(&src, b"png").unwrap();
        assert_eq!(
            add_resource_as(&dir, &src, "Share flow").unwrap(),
            "Share flow.png"
        );
        assert_eq!(
            add_resource_as(&dir, &src, "share flow.PNG").unwrap(),
            "share flow (2).PNG"
        );
        assert!(add_resource_as(&dir, &src, "../escape").is_err());
        assert!(add_resource_as(&dir, &src, ".hidden").is_err());
        assert_eq!(
            rename_resource(&dir, "Share flow.png", "Sharing v2").unwrap(),
            "Sharing v2.png"
        );
        assert!(
            rename_resource(&dir, "Sharing v2.png", "share flow (2).PNG").is_err(),
            "taken"
        );
        assert!(rename_resource(&dir, "missing.png", "x").is_err());
        assert_eq!(
            load(&dir).unwrap().files,
            ["Sharing v2.png", "share flow (2).PNG"]
        );
    }

    #[test]
    fn a_link_is_retitled() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        write_links(
            &dir,
            &[Link {
                title: "x".into(),
                url: "https://f/x".into(),
            }],
        )
        .unwrap();
        rename_link(&dir, "https://f/x", "Figma: sharing").unwrap();
        assert!(rename_link(&dir, "https://f/x", " ").is_err());
        assert!(rename_link(&dir, "https://nope", "y").is_err());
        assert_eq!(load(&dir).unwrap().links[0].title, "Figma: sharing");
    }

    #[test]
    fn slugs_and_keys_come_from_the_name() {
        assert_eq!(slug_for("Product Groups!"), "product-groups");
        assert_eq!(slug_for("  EUDR  v2 "), "eudr-v2");
        assert_eq!(key_for("Product Groups"), "PG");
        assert_eq!(key_for("payments"), "PA");
        assert_eq!(key_for("a b c d e"), "ABCD");
    }

    #[test]
    fn progress_is_read_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pg");
        sample(&dir);
        for date in ["2026-11-02", "2026-11-03"] {
            write_progress(
                &dir,
                &Progress {
                    login: "aza-iov42".into(),
                    date: date.into(),
                    body: "did".into(),
                },
            )
            .unwrap();
        }
        let p = load(&dir).unwrap();
        assert_eq!(p.progress[0].date, "2026-11-03");
    }
}
