use anyhow::Context;

use crate::app::App;
use crate::style;

/// The new config.toml: commented-out examples in this platform's terms.
const CONFIG_TEMPLATE: &str = if cfg!(windows) {
    WINDOWS_TEMPLATE
} else {
    UNIX_TEMPLATE
};

const WINDOWS_TEMPLATE: &str = r#"# Hydra global settings. Environments live in envs/<name>/env.toml.
# default_shell = "pwsh"                          # or "bash"
# shells = ["pwsh", "bash"]                       # shells that get Windows Terminal profiles
# git_bash = "C:/Program Files/Git/bin/bash.exe"

[bindings]
# "E:/work/**" = "work"
"#;

const UNIX_TEMPLATE: &str = r#"# Hydra global settings. Environments live in envs/<name>/env.toml.
# default_shell = "zsh"                           # or "bash" or "pwsh"

[bindings]
# "/Users/you/work/**" = "work"
"#;

pub fn run(app: &App) -> anyhow::Result<i32> {
    let p = &app.paths;
    for d in [
        p.root().to_path_buf(),
        p.envs_dir(),
        p.base_dir(),
        p.state_root(),
    ] {
        std::fs::create_dir_all(&d).with_context(|| format!("can't create {}", d.display()))?;
    }
    let cfg = p.config_file();
    if cfg.exists() {
        anstream::println!("already set up at {}", p.root().display());
    } else {
        std::fs::write(&cfg, CONFIG_TEMPLATE)?;
        anstream::println!("{}", style::ok(format!("created {}", cfg.display())));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_template_suits_the_platform() {
        toml::from_str::<toml::Table>(CONFIG_TEMPLATE).expect("the template is valid TOML");
        if cfg!(windows) {
            assert!(CONFIG_TEMPLATE.contains("# default_shell = \"pwsh\""));
            assert!(CONFIG_TEMPLATE.contains("Windows Terminal profiles"));
            assert!(CONFIG_TEMPLATE.contains("# \"E:/work/**\" = \"work\""));
        } else {
            assert!(
                CONFIG_TEMPLATE.contains("# default_shell = \"zsh\""),
                "{CONFIG_TEMPLATE}"
            );
            for windows_only in ["Windows Terminal", "git_bash", "C:/", "E:/", "shells ="] {
                assert!(
                    !CONFIG_TEMPLATE.contains(windows_only),
                    "{windows_only:?} in {CONFIG_TEMPLATE}"
                );
            }
            assert!(CONFIG_TEMPLATE.contains("[bindings]"), "{CONFIG_TEMPLATE}");
        }
    }
}
