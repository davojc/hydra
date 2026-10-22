//! The file tree's model (spec §7.3): folders read one level at a time when first shown,
//! ignored files included (the view mutes them), and the shell text for inserting a path or changing folder.
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use hydra_platform::shell::{ShellKind, pwsh_quote, sh_quote};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// A folder's entries: folders first, then by name ignoring case; everything but `.git`,
/// what `.gitignore` excludes included. One level only, so a big folder costs nothing until
/// it is opened.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != ".git")
        .map(|e| Entry {
            // Follows links, so a linked folder opens like a folder.
            is_dir: e.path().is_dir(),
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// A visible row: an entry at its depth below the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub entry: Entry,
    pub depth: usize,
    pub expanded: bool,
}

/// The tree under a root: which folders are open, and their listings, read when first shown.
#[derive(Debug, Default)]
pub struct FileTree {
    root: PathBuf,
    open: BTreeSet<PathBuf>,
    listings: HashMap<PathBuf, Vec<Entry>>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ..Default::default()
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Moves to a new root (the focused pane's folder), starting closed. Returns whether it
    /// changed.
    pub fn set_root(&mut self, root: PathBuf) -> bool {
        if root == self.root {
            return false;
        }
        *self = FileTree::new(root);
        true
    }

    pub fn toggle(&mut self, dir: &Path) {
        if !self.open.remove(dir) {
            self.open.insert(dir.to_path_buf());
        }
    }

    /// The open folders, for the watcher.
    pub fn open_dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.open.iter()
    }

    /// Forgets a folder's listing so it is read again (after a change on disk).
    pub fn refresh(&mut self, dir: &Path) {
        self.listings.remove(dir);
    }

    /// The root's entries, with each open folder's entries below it.
    pub fn rows(&mut self) -> Vec<Row> {
        let mut out = Vec::new();
        let root = self.root.clone();
        self.push_rows(&root, 0, &mut out);
        out
    }

    fn push_rows(&mut self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let entries = self
            .listings
            .entry(dir.to_path_buf())
            .or_insert_with(|| list_dir(dir))
            .clone();
        for entry in entries {
            let expanded = entry.is_dir && self.open.contains(&entry.path);
            let path = entry.path.clone();
            out.push(Row {
                entry,
                depth,
                expanded,
            });
            if expanded {
                self.push_rows(&path, depth + 1, out);
            }
        }
    }
}

/// The command that opens `path` in `editor` (a program name or path, found on `search` like
/// a shell would, so `.cmd` launchers such as VS Code's `code` work). It shows no console.
pub fn editor_command(
    editor: &str,
    path: &Path,
    search: &std::ffi::OsStr,
    pathext: Option<&std::ffi::OsStr>,
) -> std::process::Command {
    let program = hydra_platform::process::resolve_program(editor, search, pathext)
        .unwrap_or_else(|| PathBuf::from(editor));
    // Run directly, `.cmd` included: Rust quotes a batch file's arguments safely, where
    // `cmd /c` would mangle quotes around paths with spaces and obey `&` in a name.
    let mut cmd = std::process::Command::new(program);
    cmd.arg(path);
    hydra_platform::process::hide_window(&mut cmd);
    cmd
}

/// A path as one word for the shell: single-quoted for pwsh, single-quoted with forward
/// slashes for Git Bash, and single-quoted as it is for zsh.
pub fn quote_path(shell: ShellKind, path: &Path) -> String {
    let text = path.to_string_lossy();
    match shell {
        ShellKind::Pwsh => pwsh_quote(&text),
        ShellKind::Bash => sh_quote(&text.replace('\\', "/")),
        ShellKind::Zsh => sh_quote(&text),
    }
}

/// What double-clicking a folder types: change folder, then Enter. pwsh uses `-LiteralPath`
/// so `[` and `]` in a name aren't wildcards.
pub fn cd_command(shell: ShellKind, dir: &Path) -> String {
    match shell {
        ShellKind::Pwsh => format!("Set-Location -LiteralPath {}\r", quote_path(shell, dir)),
        ShellKind::Bash | ShellKind::Zsh => format!("cd {}\r", quote_path(shell, dir)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// src/ (with inner.rs), target/ (ignored), Zeta/, .git/, and files (x.log ignored).
    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for dir in ["src", "target", "Zeta", ".git"] {
            std::fs::create_dir_all(p.join(dir)).unwrap();
        }
        for file in [
            "b.txt",
            "A.md",
            "x.log",
            ".env",
            "src/inner.rs",
            "target/out.bin",
        ] {
            std::fs::write(p.join(file), "").unwrap();
        }
        std::fs::write(p.join(".gitignore"), "target/\n*.log\n").unwrap();
        d
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn a_listing_is_one_level_folders_first_with_ignored_files_but_not_git() {
        let d = project();
        let entries = list_dir(d.path());
        assert_eq!(
            names(&entries),
            vec![
                "src",
                "target",
                "Zeta",
                ".env",
                ".gitignore",
                "A.md",
                "b.txt",
                "x.log"
            ]
        );
        assert!(entries[0].is_dir && !entries[3].is_dir);
        assert_eq!(entries[0].path, d.path().join("src"));
    }

    #[test]
    fn open_folders_show_their_entries_below_them() {
        let d = project();
        let mut tree = FileTree::new(d.path().to_path_buf());
        assert_eq!(tree.rows().len(), 8);
        tree.toggle(&d.path().join("src"));
        let rows = tree.rows();
        assert_eq!(rows[0].entry.name, "src");
        assert!(rows[0].expanded);
        assert_eq!(
            (rows[1].entry.name.as_str(), rows[1].depth),
            ("inner.rs", 1)
        );
        assert_eq!(tree.open_dirs().count(), 1);
        tree.toggle(&d.path().join("src"));
        assert_eq!(tree.rows().len(), 8);
    }

    #[test]
    fn listings_are_kept_until_refreshed() {
        let d = project();
        let mut tree = FileTree::new(d.path().to_path_buf());
        assert_eq!(tree.rows().len(), 8);
        std::fs::write(d.path().join("new.txt"), "").unwrap();
        assert_eq!(tree.rows().len(), 8);
        tree.refresh(d.path());
        assert_eq!(tree.rows().len(), 9);
    }

    #[test]
    fn a_new_root_starts_closed() {
        let d = project();
        let mut tree = FileTree::new(d.path().to_path_buf());
        tree.toggle(&d.path().join("src"));
        assert!(!tree.set_root(d.path().to_path_buf()));
        assert!(tree.set_root(d.path().join("src")));
        assert_eq!(tree.root(), d.path().join("src"));
        assert_eq!(tree.open_dirs().count(), 0);
        assert_eq!(
            names(&tree.rows().into_iter().map(|r| r.entry).collect::<Vec<_>>()),
            vec!["inner.rs"]
        );
    }

    #[test]
    fn paths_are_quoted_for_each_shell() {
        // pwsh reads ‘ ’ ‚ ‛ as quotes too: each is doubled.
        let curly = Path::new("C:\\Downloads\\David’s notes.txt");
        assert_eq!(
            quote_path(ShellKind::Pwsh, curly),
            "'C:\\Downloads\\David’’s notes.txt'"
        );
        let p = Path::new("C:\\work\\it's [here]");
        assert_eq!(quote_path(ShellKind::Pwsh, p), "'C:\\work\\it''s [here]'");
        assert_eq!(quote_path(ShellKind::Bash, p), "'C:/work/it'\\''s [here]'");
        assert_eq!(
            cd_command(ShellKind::Pwsh, p),
            "Set-Location -LiteralPath 'C:\\work\\it''s [here]'\r"
        );
        assert_eq!(
            cd_command(ShellKind::Bash, p),
            "cd 'C:/work/it'\\''s [here]'\r"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_editor_script_gets_the_path_whole() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("my editor");
        std::fs::create_dir_all(&bin).unwrap();
        // Writes its first argument, as given, next to itself.
        let ed = bin.join("ed");
        std::fs::write(
            &ed,
            "#!/bin/sh\nprintf '%s' \"$1\" > \"$(dirname \"$0\")/out.txt\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&ed, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = d.path().join("my dir").join("a&b.txt");
        let status = editor_command("ed", &file, bin.as_os_str(), None)
            .status()
            .unwrap();
        assert!(status.success());
        let got = std::fs::read_to_string(bin.join("out.txt")).unwrap_or_default();
        assert_eq!(got, file.display().to_string());
    }

    #[cfg(windows)]
    #[test]
    fn a_cmd_editor_gets_the_path_whole() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("my editor");
        std::fs::create_dir_all(&bin).unwrap();
        // Writes its first argument, as given, next to itself.
        std::fs::write(
            bin.join("ed.cmd"),
            "@echo %1> \"%~dp0out.txt\"
",
        )
        .unwrap();
        let file = d.path().join("my dir").join("a&b.txt");
        let status = editor_command("ed", &file, bin.as_os_str(), Some(".CMD".as_ref()))
            .status()
            .unwrap();
        assert!(status.success());
        let got = std::fs::read_to_string(bin.join("out.txt")).unwrap_or_default();
        assert_eq!(got.trim(), format!("\"{}\"", file.display()));
    }
}
