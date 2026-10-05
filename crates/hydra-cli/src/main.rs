mod app;
mod cli;
mod commands;
mod prompt;
mod release;
mod style;

use clap::Parser;

/// `HYDRA_VERSION` at compile time (release builds), else `<Cargo version>-dev`.
pub const VERSION: &str = match option_env!("HYDRA_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

fn main() {
    remove_old_exe();
    let cli = cli::Cli::parse();
    exit_with(commands::run(cli.command));
}

fn exit_with(result: anyhow::Result<i32>) -> ! {
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            print_error(&e);
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

/// The fatal error in red; a launch failure's `-> fix` lines are dimmed.
fn print_error(e: &anyhow::Error) {
    let text = format!("hydra: {e:#}");
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            anstream::eprintln!("{}", style::error(line));
        } else if line.trim_start().starts_with("->") {
            anstream::eprintln!("{}", style::dim(line));
        } else {
            anstream::eprintln!("{line}");
        }
    }
}
