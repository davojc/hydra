//! The environment's accounts, as shown when its name in the status bar is clicked: one row
//! per provider check, info rows included.
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

#[cfg(test)]
mod tests {
    use super::*;

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
