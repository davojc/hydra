use anyhow::Context;

use crate::app::App;

const CONFIG_TEMPLATE: &str = r#"# Hydra global settings. Environments live in envs/<name>/env.toml.
# default_shell = "pwsh"                          # or "bash"
# shells = ["pwsh", "bash"]                       # shells that get Windows Terminal profiles
# git_bash = "C:/Program Files/Git/bin/bash.exe"

[bindings]
# "E:/work/**" = "work"
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
        println!("already set up at {}", p.root().display());
    } else {
        std::fs::write(&cfg, CONFIG_TEMPLATE)?;
        println!("created {}", cfg.display());
    }
    Ok(0)
}
