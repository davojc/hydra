//! hydra-setup's command line: `--silent`, `--uninstall`, `--dir <folder>`, `--from <folder>`,
//! `--desktop`.
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    pub silent: bool,
    pub uninstall: bool,
    pub dir: Option<PathBuf>,
    pub from: Option<PathBuf>,
    pub desktop: bool,
}

/// The flags in `args` (without the program name).
pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{flag} needs a folder"))
        };
        match a.as_str() {
            "--silent" => out.silent = true,
            "--uninstall" => out.uninstall = true,
            "--desktop" => out.desktop = true,
            "--dir" => out.dir = Some(value("--dir")?),
            "--from" => out.from = Some(value("--from")?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_read() {
        let a = |v: &[&str]| parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let got = a(&["--silent", "--dir", r"C:\t", "--from", "dist", "--desktop"]).unwrap();
        assert!(got.silent && got.desktop && !got.uninstall);
        assert_eq!(got.dir.as_deref(), Some(std::path::Path::new(r"C:\t")));
        assert_eq!(got.from.as_deref(), Some(std::path::Path::new("dist")));
        assert!(a(&["--uninstall", "--silent"]).unwrap().uninstall);
        assert_eq!(a(&[]).unwrap(), Args::default());
        assert!(a(&["--dir"]).is_err(), "a flag missing its value");
        assert!(a(&["--nope"]).is_err());
    }
}
