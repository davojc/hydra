//! Binding resolution: which environment a folder belongs to.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where a binding came from: the rule pattern as written, or the folder holding a `.hydra` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Rule(String),
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub env: String,
    pub source: Source,
}

impl Binding {
    pub fn describe(&self) -> String {
        match &self.source {
            Source::Rule(p) => format!("rule {p}"),
            Source::File(dir) => format!(".hydra in {}", dir.display()),
        }
    }
}

#[derive(Deserialize)]
struct HydraFile {
    env: String,
}

/// A backslash becomes `/`, a verbatim `\\?\` (or `\\?\UNC\`) prefix is dropped, trailing `/` is
/// trimmed, and on Windows the result is lowercased.
pub fn normalize(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = match s.strip_prefix("//?/") {
        Some(rest) => match rest.get(..4) {
            Some(unc) if unc.eq_ignore_ascii_case("UNC/") => format!("//{}", &rest[4..]),
            _ => rest.to_string(),
        },
        None => s,
    };
    let s = s.trim_end_matches('/');
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

fn segments(p: &str) -> Vec<String> {
    normalize(p)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn seg_match(pat: &[char], text: &[char]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|i| seg_match(rest, &text[i..])),
        Some(('?', rest)) => !text.is_empty() && seg_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && seg_match(rest, &text[1..]),
    }
}

fn match_segments(pat: &[String], path: &[String]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((p, rest)) if p == "**" => (0..=path.len()).any(|i| match_segments(rest, &path[i..])),
        Some((p, rest)) => match path.split_first() {
            Some((s, path_rest)) => {
                let pc: Vec<char> = p.chars().collect();
                let sc: Vec<char> = s.chars().collect();
                seg_match(&pc, &sc) && match_segments(rest, path_rest)
            }
            None => false,
        },
    }
}

/// Glob match on normalised paths: `**` spans any number of segments, `*` and `?` stay in one.
pub fn glob_path(pattern: &str, path: &str) -> bool {
    match_segments(&segments(pattern), &segments(path))
}

/// Number of leading pattern segments with no `*` or `?`.
pub fn literal_depth(pattern: &str) -> usize {
    segments(pattern)
        .iter()
        .take_while(|s| !s.contains('*') && !s.contains('?'))
        .count()
}

fn read_hydra_file(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".hydra")).ok()?;
    toml::from_str::<HydraFile>(&text).ok().map(|f| f.env)
}

/// Expand Windows 8.3 short names (`RUNNER~1`) to long ones, so a folder
/// matches however it was spelled. Paths that don't exist come back unchanged.
pub fn long_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    if p.to_string_lossy().contains('~') {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetLongPathNameW(short: *const u16, long: *mut u16, len: u32) -> u32;
        }
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: `wide` is NUL-terminated and `buf` holds `buf.len()` u16s.
            let n = unsafe { GetLongPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
                as usize;
            if n == 0 {
                break;
            }
            if n < buf.len() {
                return std::ffi::OsString::from_wide(&buf[..n]).into();
            }
            buf.resize(n, 0);
        }
    }
    p.to_path_buf()
}

/// `pattern` with its literal leading folders expanded by [`long_path`].
fn long_pattern(pattern: &str) -> String {
    if !pattern.contains('~') {
        return pattern.to_string();
    }
    let segs: Vec<&str> = pattern.split(['/', '\\']).collect();
    let n = segs
        .iter()
        .take_while(|s| !s.contains('*') && !s.contains('?'))
        .count();
    let prefix = segs[..n].join("/");
    let mut out = long_path(Path::new(&prefix)).to_string_lossy().into_owned();
    for s in &segs[n..] {
        out.push('/');
        out.push_str(s);
    }
    out
}

/// Resolve a folder to its environment from config rules and `.hydra` files.
pub fn resolve(dir: &Path, rules: &BTreeMap<String, String>) -> Option<Binding> {
    let dir = &long_path(dir);
    let dir_str = normalize(&dir.to_string_lossy());
    let rule = rules
        .iter()
        .filter(|(pattern, _)| glob_path(&long_pattern(pattern), &dir_str))
        .max_by_key(|(pattern, _)| (literal_depth(pattern), pattern.len()));
    // An unreadable or invalid .hydra file is ignored and the walk continues upward.
    let file = dir
        .ancestors()
        .find_map(|folder| read_hydra_file(folder).map(|env| (folder, env)));
    let rule_depth = rule.map(|(p, _)| literal_depth(p));
    match (file, rule) {
        (Some((folder, env)), r)
            if r.is_none()
                || segments(&folder.to_string_lossy()).len() >= rule_depth.unwrap_or(0) =>
        {
            Some(Binding {
                env,
                source: Source::File(folder.to_path_buf()),
            })
        }
        (_, Some((pattern, env))) => Some(Binding {
            env: env.clone(),
            source: Source::Rule(pattern.clone()),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rules(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(p, e)| (p.to_string(), e.to_string()))
            .collect()
    }

    /// The 8.3 short form of `p`, from cmd's `%~sI`; None when the volume has
    /// short names turned off.
    #[cfg(windows)]
    fn short_form(p: &Path) -> Option<std::path::PathBuf> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/d /c for %I in (\"{}\") do @echo %~sI",
                p.display()
            ))
            .output()
            .ok()?;
        let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
        short.contains('~').then(|| short.into())
    }

    #[cfg(windows)]
    #[test]
    fn short_and_long_spellings_match_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let long = tmp.path().join("a rather long folder name");
        std::fs::create_dir_all(&long).unwrap();
        let Some(short) = short_form(&long) else {
            return;
        };
        let long_rule = format!("{}/**", long.display());
        let short_rule = format!("{}/**", short.display());
        assert_eq!(
            resolve(&short, &rules(&[(&long_rule, "work")])).map(|b| b.env),
            Some("work".into())
        );
        assert_eq!(
            resolve(&long, &rules(&[(&short_rule, "work")])).map(|b| b.env),
            Some("work".into())
        );
    }

    #[test]
    fn normalize_drops_verbatim_prefixes() {
        assert_eq!(normalize(r"\\?\C:\Work\Repo\"), normalize("C:/Work/Repo"));
        assert_eq!(
            normalize(r"\\?\UNC\server\share\x"),
            normalize("//server/share/x")
        );
        assert_eq!(normalize(r"\\server\share"), normalize("//server/share"));
        assert!(glob_path("c:/work/**", r"\\?\C:\work\a"));
    }

    #[test]
    fn globs() {
        assert!(glob_path("e:/work/**", "e:/work"));
        assert!(glob_path("e:/work/**", "e:/work/a/b/c"));
        assert!(!glob_path("e:/work/**", "e:/workshop/a"));
        assert!(glob_path("e:/*/repo", "e:/work/repo"));
        assert!(!glob_path("e:/*/repo", "e:/a/b/repo"));
        assert!(glob_path("e:/work", "e:/work"));
        assert!(!glob_path("e:/work", "e:/work/a"));
        assert_eq!(literal_depth("e:/work/**"), 2);
        assert_eq!(literal_depth("e:/*/repo"), 1);
    }

    #[test]
    fn matching_ignores_case_and_slashes() {
        let r = rules(&[("E:/Work/**", "work")]);
        let b = resolve(Path::new(r"e:\work\Repo\src"), &r).unwrap();
        assert_eq!(b.env, "work");
        assert_eq!(b.source, Source::Rule("E:/Work/**".into()));
    }

    #[test]
    fn closest_rule_wins() {
        let r = rules(&[("E:/work/**", "work"), ("E:/work/client/**", "client")]);
        assert_eq!(
            resolve(Path::new("E:/work/client/x"), &r).unwrap().env,
            "client"
        );
        assert_eq!(resolve(Path::new("E:/work/other"), &r).unwrap().env, "work");
        assert!(resolve(Path::new("E:/elsewhere"), &r).is_none());
    }

    #[test]
    fn hydra_file_beats_a_broader_rule_and_loses_to_a_deeper_one() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("work").join("acme");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(".hydra"), "env = \"client\"\n").unwrap();
        let base = normalize(&dir.path().to_string_lossy());
        let r = rules(&[(&format!("{base}/work/**"), "work")]);
        let b = resolve(&repo.join("src"), &r).unwrap();
        assert_eq!(b.env, "client");
        assert!(matches!(b.source, Source::File(_)));
        let deeper = rules(&[(&format!("{base}/work/acme/src/**"), "work")]);
        assert_eq!(resolve(&repo.join("src"), &deeper).unwrap().env, "work");
    }

    #[test]
    fn broken_hydra_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hydra"), "not toml [").unwrap();
        assert!(resolve(dir.path(), &BTreeMap::new()).is_none());
    }
}
