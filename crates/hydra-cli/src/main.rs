mod app;
mod cli;
mod commands;
mod prompt;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    match commands::run(cli.command) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("hydra: {e:#}");
            std::process::exit(1);
        }
    }
}
