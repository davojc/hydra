//! A command-line stand-in for the phone, for trying the link against a running hydraui.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hydra_link::client::{Client, LinkError};
use hydra_link::pairing::PairingOffer;
use hydra_link::protocol::ClientMsg;
use tokio::sync::broadcast::error::RecvError;

#[derive(Parser)]
#[command(name = "link-probe", about = "A command-line phone for the hydra link")]
struct Cli {
    /// Folder holding this probe's key and the paired desktop id.
    #[arg(long)]
    state: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pair with the desktop that showed this offer string.
    Pair { offer: String },
    /// Print every message the desktop sends, one JSON line each, until Ctrl+C.
    Watch,
    /// Send one ClientMsg (JSON, e.g. '{"type":"snapshot"}') and print the outcome.
    Send { msg: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("link-probe: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    fs::create_dir_all(&cli.state).map_err(|e| format!("state folder: {e}"))?;
    let key = load_or_create_key(&cli.state.join("key"))?;
    match cli.command {
        Command::Pair { offer } => {
            let offer = PairingOffer::decode(&offer).ok_or("that is not a pairing offer")?;
            let (client, word) = Client::pair(key, &offer, true).await.map_err(show)?;
            fs::write(cli.state.join("desktop"), &offer.endpoint)
                .map_err(|e| format!("saving the desktop id: {e}"))?;
            println!("paired; matching word: {word}");
            drop(client);
        }
        Command::Watch => {
            let client = connect(&cli.state, key).await?;
            let mut events = client.events();
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    event = events.recv() => match event {
                        Ok(msg) => println!("{}", json(&msg)?),
                        Err(RecvError::Lagged(n)) => eprintln!("link-probe: missed {n} messages"),
                        Err(RecvError::Closed) => {
                            eprintln!("link-probe: the connection closed");
                            break;
                        }
                    },
                }
            }
        }
        Command::Send { msg } => {
            let msg: ClientMsg =
                serde_json::from_str(&msg).map_err(|e| format!("bad message: {e}"))?;
            let client = connect(&cli.state, key).await?;
            let outcome = client.request(msg).await.map_err(show)?;
            println!("{}", json(&outcome)?);
        }
    }
    Ok(())
}

async fn connect(state: &Path, key: [u8; 32]) -> Result<Client, String> {
    let desktop = fs::read_to_string(state.join("desktop"))
        .map_err(|_| "not paired yet: run `pair <offer>` first".to_string())?;
    Client::connect(key, desktop.trim(), true)
        .await
        .map_err(show)
}

fn show(e: LinkError) -> String {
    e.to_string()
}

fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

/// The probe's secret key: 32 random bytes kept as hex, made on first use.
fn load_or_create_key(path: &Path) -> Result<[u8; 32], String> {
    if let Ok(text) = fs::read_to_string(path) {
        let text = text.trim();
        if text.len() == 64 {
            let mut key = [0u8; 32];
            for (i, byte) in key.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
                    .map_err(|_| "the key file is not hex".to_string())?;
            }
            return Ok(key);
        }
        return Err("the key file is malformed".into());
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| format!("random key: {e}"))?;
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    fs::write(path, hex).map_err(|e| format!("saving the key: {e}"))?;
    Ok(key)
}
