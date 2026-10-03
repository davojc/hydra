use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hydra", version, about = "Terminals with their own identities")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Create ~/.hydra
    Init,
    /// Manage environments
    Env {
        #[command(subcommand)]
        command: EnvCmd,
    },
    /// Store or remove secrets in the system credential store
    Secret {
        #[command(subcommand)]
        command: SecretCmd,
    },
    /// Open a shell in an environment
    Shell {
        /// Environment name
        env: Option<String>,
        /// pwsh or bash (default: default_shell in config.toml, else pwsh)
        #[arg(long)]
        shell: Option<String>,
        /// Folder to open in
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Run one command in an environment: hydra run <env> -- <command...>
    Run {
        env: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Sign in to a tool inside an environment, e.g. hydra auth github work
    Auth { provider: String, env: String },
    /// Show which account each tool is actually using
    Whoami {
        /// Environment to check (default: the current hydra terminal's)
        #[arg(long)]
        env: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnvCmd {
    /// Create an environment
    New { name: String },
    /// List environments
    List,
    /// Delete an environment, its secrets and (after asking) its saved logins
    Rm {
        name: String,
        /// Also delete saved logins without asking
        #[arg(long)]
        yes: bool,
    },
    /// Open env.toml in your editor and check it when you close the editor
    Edit { name: String },
    /// Rename an environment and update its bindings, secrets and saved logins
    Rename { old: String, new: String },
}

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Store a secret, e.g. `hydra secret set work/linear` (the value is read from a hidden prompt)
    Set { path: String },
    /// Remove a secret
    Rm { path: String },
}
