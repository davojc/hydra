//! Pure logic for `hydra update`: reading GitHub's release JSON, comparing versions,
//! checksum files and process lists. No network or file system here.

use anyhow::Context;
use semver::Version;

/// The release asset `hydra update` installs.
pub const EXE_ASSET: &str = "hydra-x86_64-pc-windows-msvc.exe";

pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

pub struct Asset {
    pub name: String,
    pub url: String,
}

/// Download URLs of the Windows exe and its .sha256 file.
pub struct WindowsBuild {
    pub exe_url: String,
    pub sha256_url: String,
}

impl Release {
    /// Reads `tag_name` and `assets[] {name, browser_download_url}` from GitHub's JSON.
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        let v: serde_json::Value =
            serde_json::from_str(json).context("GitHub sent release info hydra can't read")?;
        let tag = v["tag_name"]
            .as_str()
            .context("GitHub's release info has no tag_name")?
            .to_string();
        let assets = v["assets"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|a| {
                Some(Asset {
                    name: a["name"].as_str()?.to_string(),
                    url: a["browser_download_url"].as_str()?.to_string(),
                })
            })
            .collect();
        Ok(Self { tag, assets })
    }

    /// The version in the tag, without its leading `v`.
    pub fn version(&self) -> anyhow::Result<Version> {
        let s = self.tag.strip_prefix('v').unwrap_or(&self.tag);
        Version::parse(s).with_context(|| format!("release tag {} isn't a version", self.tag))
    }

    pub fn windows_build(&self) -> anyhow::Result<WindowsBuild> {
        let url = |name: &str| {
            self.assets
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.url.clone())
        };
        match (url(EXE_ASSET), url(&format!("{EXE_ASSET}.sha256"))) {
            (Some(exe_url), Some(sha256_url)) => Ok(WindowsBuild {
                exe_url,
                sha256_url,
            }),
            _ => anyhow::bail!("release {} has no Windows build", self.tag),
        }
    }
}

/// True when `latest` should replace `current`. `0.4.0-dev` is older than `0.4.0`.
pub fn is_newer(latest: &Version, current: &Version) -> bool {
    latest > current
}

/// The checksum in a `.sha256` file: its first whitespace-separated token, lowercased.
pub fn parse_checksum(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    (token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| token.to_ascii_lowercase())
}

/// Image names `hydra update` treats as a running hydra: the exe, and an old one still
/// running from a previous update (the same names scripts/deploy.ps1 checks).
#[cfg_attr(not(windows), allow(dead_code))]
pub const HYDRA_IMAGES: [&str; 2] = ["hydra.exe", "hydra.old.exe"];

/// PIDs of hydra processes in `tasklist /FO CSV /NH` output, whose lines look like
/// `"hydra.exe","1234","Console","1","9,000 K"`. Anything hydra can't read is an error,
/// so the caller refuses to update rather than guess.
#[cfg_attr(not(windows), allow(dead_code))] // used on Windows; tested everywhere
pub fn hydra_pids_from_tasklist(text: &str) -> Result<Vec<u32>, String> {
    let mut pids = Vec::new();
    let mut lines = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        lines += 1;
        if line.starts_with("INFO:") {
            continue; // "No tasks are running which match the specified criteria."
        }
        let fields: Vec<&str> = line.split("\",\"").collect();
        let (Some(name), Some(pid)) = (fields[0].strip_prefix('"'), fields.get(1)) else {
            return Err(format!("unexpected tasklist line: {line}"));
        };
        let pid: u32 = pid
            .parse()
            .map_err(|_| format!("unexpected tasklist line: {line}"))?;
        if HYDRA_IMAGES.iter().any(|i| i.eq_ignore_ascii_case(name)) {
            pids.push(pid);
        }
    }
    if lines == 0 {
        return Err("tasklist printed nothing".into());
    }
    Ok(pids)
}

/// PIDs from `pgrep -x 'hydra|hydra\.old'`: exit 0 lists them one per line, exit 1 means
/// none are running, anything else (or output hydra can't read) is an error.
#[cfg_attr(windows, allow(dead_code))] // used off Windows; tested everywhere
pub fn hydra_pids_from_pgrep(code: Option<i32>, stdout: &str) -> Result<Vec<u32>, String> {
    match code {
        Some(1) => Ok(Vec::new()),
        Some(0) => {
            let pids = stdout
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| l.parse().map_err(|_| format!("unexpected pgrep line: {l}")))
                .collect::<Result<Vec<u32>, String>>()?;
            if pids.is_empty() {
                return Err("pgrep found processes but printed none".into());
            }
            Ok(pids)
        }
        Some(c) => Err(format!("pgrep exited with {c}")),
        None => Err("pgrep was stopped by a signal".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newer_versions_compare_by_semver() {
        assert!(is_newer(&v("0.4.0"), &v("0.4.0-dev")));
        assert!(is_newer(&v("0.4.10"), &v("0.4.9")));
        assert!(is_newer(&v("0.5.0"), &v("0.4.10")));
        assert!(!is_newer(&v("0.4.0"), &v("0.4.0")));
        assert!(!is_newer(&v("0.3.0"), &v("0.4.0-dev")));
        assert!(!is_newer(&v("0.4.0-dev"), &v("0.4.0")));
    }

    #[test]
    fn checksum_is_the_first_token_lowercased() {
        assert_eq!(
            parse_checksum(&format!("{SHA}  {EXE_ASSET}\n")).as_deref(),
            Some(SHA)
        );
        assert_eq!(
            parse_checksum(&format!("  {}\r\n", SHA.to_uppercase())).as_deref(),
            Some(SHA)
        );
        assert_eq!(parse_checksum(SHA).as_deref(), Some(SHA));
    }

    #[test]
    fn bad_checksum_files_give_none() {
        assert_eq!(parse_checksum(""), None);
        assert_eq!(parse_checksum("   \n"), None);
        assert_eq!(parse_checksum("not-a-hash  hydra.exe"), None);
        assert_eq!(parse_checksum(&SHA[..63]), None);
    }

    #[test]
    fn release_json_gives_tag_version_and_windows_build() {
        let r = Release::parse(&format!(
            r#"{{"tag_name":"v0.4.10","name":"x","assets":[
                {{"name":"{EXE_ASSET}","browser_download_url":"https://e/exe"}},
                {{"name":"{EXE_ASSET}.sha256","browser_download_url":"https://e/sha"}}]}}"#
        ))
        .unwrap();
        assert_eq!(r.tag, "v0.4.10");
        assert_eq!(r.version().unwrap(), v("0.4.10"));
        let b = r.windows_build().unwrap();
        assert_eq!(
            (b.exe_url.as_str(), b.sha256_url.as_str()),
            ("https://e/exe", "https://e/sha")
        );
    }

    #[test]
    fn release_without_checksum_has_no_windows_build() {
        let r = Release::parse(&format!(
            r#"{{"tag_name":"v0.4.1","assets":[{{"name":"{EXE_ASSET}","browser_download_url":"u"}}]}}"#
        ))
        .unwrap();
        let err = r.windows_build().err().unwrap().to_string();
        assert_eq!(err, "release v0.4.1 has no Windows build");
        let r = Release::parse(r#"{"tag_name":"v0.4.1"}"#).unwrap();
        assert!(r.windows_build().is_err());
    }

    #[test]
    fn release_json_errors() {
        assert!(Release::parse("not json").is_err());
        assert!(Release::parse(r#"{"assets":[]}"#).is_err());
        let r = Release::parse(r#"{"tag_name":"nightly"}"#).unwrap();
        assert!(r.version().is_err());
    }

    #[test]
    fn tasklist_lists_hydra_and_hydra_old() {
        let out = "\r\n\"System\",\"4\",\"Services\",\"0\",\"144 K\"\r\n\
                   \"hydra.exe\",\"1234\",\"Console\",\"1\",\"9,000 K\"\r\n\
                   \"pwsh.exe\",\"77\",\"Console\",\"1\",\"80,000 K\"\r\n\
                   \"HYDRA.OLD.EXE\",\"88\",\"Console\",\"1\",\"8,000 K\"\r\n\
                   \"hydra-tool.exe\",\"99\",\"Console\",\"1\",\"8,000 K\"\r\n";
        assert_eq!(hydra_pids_from_tasklist(out), Ok(vec![1234, 88]));
        assert_eq!(
            hydra_pids_from_tasklist("\"System\",\"4\",\"Services\",\"0\",\"144 K\"\r\n"),
            Ok(vec![])
        );
        assert_eq!(
            hydra_pids_from_tasklist(
                "INFO: No tasks are running which match the specified criteria.\r\n"
            ),
            Ok(vec![])
        );
    }

    #[test]
    fn tasklist_output_hydra_cant_read_is_an_error() {
        assert!(hydra_pids_from_tasklist("").is_err());
        assert!(hydra_pids_from_tasklist("ERROR: Invalid argument/option.\r\n").is_err());
        assert!(hydra_pids_from_tasklist("Image Name   PID\r\n").is_err());
        assert!(
            hydra_pids_from_tasklist("\"hydra.exe\",\"abc\",\"Console\",\"1\",\"9 K\"\r\n")
                .is_err()
        );
        assert!(hydra_pids_from_tasklist("\"hydra.exe\"\r\n").is_err());
    }

    #[test]
    fn pgrep_exit_codes() {
        assert_eq!(
            hydra_pids_from_pgrep(Some(0), "12\n345\n"),
            Ok(vec![12, 345])
        );
        assert_eq!(hydra_pids_from_pgrep(Some(1), ""), Ok(vec![]));
        assert!(hydra_pids_from_pgrep(Some(2), "").is_err());
        assert!(hydra_pids_from_pgrep(Some(3), "").is_err());
        assert!(hydra_pids_from_pgrep(None, "").is_err());
        assert!(hydra_pids_from_pgrep(Some(0), "12\nnope\n").is_err());
        assert!(hydra_pids_from_pgrep(Some(0), "").is_err());
    }
}
