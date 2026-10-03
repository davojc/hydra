use std::io::{BufRead, IsTerminal, Write};

/// Reads a secret from a hidden prompt, or from one stdin line when stdin isn't a terminal.
pub fn read_secret(prompt: &str) -> anyhow::Result<String> {
    let raw = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(prompt)?
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line
    };
    let v = raw.trim_end_matches(['\r', '\n']).to_string();
    if v.is_empty() {
        anyhow::bail!("no value entered; nothing stored");
    }
    Ok(v)
}

/// Asks a yes/no question. Without a terminal there is nobody to ask, so the answer is no.
pub fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{question} {} ", if default { "[Y/n]" } else { "[y/N]" });
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let a = line.trim().to_ascii_lowercase();
    Ok(if a.is_empty() {
        default
    } else {
        a == "y" || a == "yes"
    })
}
