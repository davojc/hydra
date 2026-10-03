use hydra_core::secret::SecretRef;

use crate::app::App;
use crate::cli::SecretCmd;
use crate::prompt;

const STORE_NAME: &str = if cfg!(windows) {
    "Windows Credential Manager"
} else {
    "the system credential store"
};

pub fn run(app: &App, cmd: SecretCmd) -> anyhow::Result<i32> {
    match cmd {
        SecretCmd::Set { path } => {
            let r = SecretRef::parse_path(&path)?;
            app.env_name(r.env.as_str())?;
            let value = prompt::read_secret(&format!(
                "value (for multi-line values like JSON, pipe them: Get-Content -Raw file | hydra secret set {}): ",
                r.path()
            ))?;
            app.store.set(&r, &value)?;
            println!("stored {} in {STORE_NAME}", r.path());
            Ok(0)
        }
        SecretCmd::Rm { path } => {
            let r = SecretRef::parse_path(&path)?;
            if app.store.delete(&r)? {
                println!("removed {}", r.path());
                Ok(0)
            } else {
                eprintln!("hydra: no secret named {}", r.path());
                Ok(1)
            }
        }
    }
}
