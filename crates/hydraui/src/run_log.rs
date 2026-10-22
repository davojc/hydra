//! `<HYDRA_HOME>/hydraui/runs/<run>.jsonl` (orchestrator spec §8): every change to a run, one JSON
//! object a line, appended as it happens. The audit trail; nothing reads it back.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// The log of run `run`. Characters other than letters, digits, `-` and `_` become `_`, so an
/// id can't leave the folder.
pub fn file_for(hydra_root: &Path, run: &str) -> PathBuf {
    let name: String = run
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    hydra_root
        .join("hydraui")
        .join("runs")
        .join(format!("{name}.jsonl"))
}

/// Appends one line: `{"at": <local time>, "event": event, ...fields}`. Callers treat a failure
/// as something to note in `hydraui.log`, never a reason to stop a run.
pub fn append(file: &Path, event: &str, fields: Value) -> std::io::Result<()> {
    let mut line = Map::new();
    line.insert("at".into(), Value::String(crate::action_log::now()));
    line.insert("event".into(), Value::String(event.into()));
    if let Value::Object(more) = fields {
        line.extend(more);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    writeln!(out, "{}", Value::Object(line))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn each_event_is_one_json_line() {
        let d = tempfile::tempdir().unwrap();
        let file = file_for(d.path(), "INTH-1-1");
        append(
            &file,
            "run_started",
            json!({"ticket": "INTH-1", "pipeline": "ticket"}),
        )
        .unwrap();
        append(&file, "report", json!({"stage": "replicate", "score": 86})).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "run_started");
        assert_eq!(lines[0]["ticket"], "INTH-1");
        assert_eq!(lines[1]["score"], 86);
        assert!(
            lines[1]["at"].as_str().unwrap().len() >= 19,
            "{}",
            lines[1]["at"]
        );
    }

    #[test]
    fn a_run_id_is_a_safe_file_name() {
        let d = tempfile::tempdir().unwrap();
        let runs = d.path().join("hydraui").join("runs");
        assert_eq!(file_for(d.path(), "INTH-1-1"), runs.join("INTH-1-1.jsonl"));
        let odd = file_for(d.path(), "../x/y");
        assert_eq!(odd.parent().unwrap(), runs);
        assert!(!odd.to_string_lossy().contains(".."), "{}", odd.display());
    }
}
