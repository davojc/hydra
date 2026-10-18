//! The user PATH as text: entries split on `;`, compared ignoring case and a trailing `\`.

/// Whether two PATH entries name the same folder.
pub fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\')
        .eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

pub fn entries(path: &str) -> Vec<&str> {
    path.split(';').filter(|e| !e.is_empty()).collect()
}

pub fn has_entry(path: &str, dir: &str) -> bool {
    entries(path).into_iter().any(|e| same(e, dir))
}

/// `path` with `dir` appended, or `None` when it's already there.
pub fn add_entry(path: &str, dir: &str) -> Option<String> {
    if has_entry(path, dir) {
        return None;
    }
    let mut out = entries(path);
    out.push(dir);
    Some(out.join(";"))
}

/// `path` without `dir` (and without empty entries).
pub fn remove_entry(path: &str, dir: &str) -> String {
    entries(path)
        .into_iter()
        .filter(|e| !same(e, dir))
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;
    const DIR: &str = r"C:\Users\u\AppData\Local\Programs\hydra";

    #[test]
    fn installing_twice_adds_the_folder_once() {
        let once = add_entry(r"C:\a;C:\b", DIR).unwrap();
        assert_eq!(once, format!(r"C:\a;C:\b;{DIR}"));
        assert_eq!(add_entry(&once, DIR), None);
        assert_eq!(
            add_entry(&once, &format!(r"{}\", DIR.to_uppercase())),
            None,
            "case and slash"
        );
        assert_eq!(add_entry("", DIR).unwrap(), DIR);
    }

    #[test]
    fn removing_keeps_every_other_entry() {
        let path = format!(r"C:\a;{DIR}\;C:\Users\u\AppData\Local\Programs\hydra-old;;C:\b");
        assert_eq!(
            remove_entry(&path, DIR),
            r"C:\a;C:\Users\u\AppData\Local\Programs\hydra-old;C:\b"
        );
        assert_eq!(remove_entry(DIR, DIR), "");
        assert!(has_entry(&path, &DIR.to_lowercase()));
    }
}
