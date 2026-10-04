use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hydra", version, about = "Terminals with their own identities")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once at startup; boxing would only add noise
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
    /// Turn a tool on for an environment (writes env.toml only; sign in afterwards inside the environment)
    Add {
        /// claude, github (gh), git, aws, azure, gcloud, gws, kube, codex, gemini. Omit to list tools.
        tool: Option<String>,
        /// Environment (default: the current hydra terminal's)
        env: Option<String>,
        #[arg(long = "mcp-exclude")]
        mcp_exclude: Vec<String>,
        #[arg(long = "owner")]
        owners: Vec<String>,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        email: Option<String>,
        #[arg(long = "ssh-key")]
        ssh_key: Option<String>,
        #[arg(long = "signing-key")]
        signing_key: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        isolate: bool,
        /// gws: secret name holding credentials JSON, e.g. iov/gws-creds
        #[arg(long = "credentials-secret")]
        credentials_secret: Option<String>,
        #[arg(long = "kubeconfig")]
        kube_config: Option<String>,
        /// gemini: secret name holding the API key, e.g. iov/gemini
        #[arg(long = "api-key-secret")]
        api_key_secret: Option<String>,
    },
    /// Turn a tool off for an environment (saved logins are kept)
    Remove { tool: String, env: Option<String> },
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
