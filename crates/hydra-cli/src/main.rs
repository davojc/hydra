mod app;
mod cli;
mod commands;
mod prompt;
mod release;

use clap::Parser;

/// `HYDRA_VERSION` at compile time (release builds), else `<Cargo version>-dev`.
pub const VERSION: &str = match option_env!("HYDRA_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

fn main() {
    remove_old_exe();
    let cli = cli::Cli::parse();
    match commands::run(cli.command) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("hydra: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Finishes a previous `hydra update`: the replaced exe can only be deleted once it has stopped.
fn remove_old_exe() {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::fs::remove_file(dir.join(commands::update::OLD_EXE));
    }
}
