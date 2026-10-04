use std::io::{BufRead, IsTerminal, Read, Write};

/// Reads a secret from a hidden prompt, or all of stdin when stdin isn't a terminal
/// (so multi-line values such as JSON can be piped in).
pub fn read_secret(prompt: &str) -> anyhow::Result<String> {
    let v = if std::io::stdin().is_terminal() {
        let raw = rpassword::prompt_password(prompt)?;
        raw.trim_end_matches(['\r', '\n']).to_string()
    } else {
        let mut all = String::new();
        std::io::stdin().lock().read_to_string(&mut all)?;
        strip_one_newline(&all).to_string()
    };
    if v.is_empty() {
        anyhow::bail!("no value entered; nothing stored");
    }
    Ok(v)
}

/// Drops exactly one trailing `\r\n` or `\n`.
fn strip_one_newline(s: &str) -> &str {
    s.strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .unwrap_or(s)
}

/// Asks a yes/no question. Without a terminal there is nobody to ask, so the answer is no.
pub fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    anstream::print!("{question} {} ", if default { "[Y/n]" } else { "[y/N]" });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_exactly_one_trailing_newline() {
        assert_eq!(strip_one_newline("a\nb\r\n"), "a\nb");
        assert_eq!(strip_one_newline("a\n\n"), "a\n");
        assert_eq!(strip_one_newline("a"), "a");
    }
}
