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
}

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Store a secret, e.g. `hydra secret set work/linear` (the value is read from a hidden prompt)
    Set { path: String },
    /// Remove a secret
    Rm { path: String },
}
