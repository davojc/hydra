//! Finds the shell-integration sequences in PTY output: OSC 7 (current folder) and OSC 133
//! (prompt marks). Everything else is left to `alacritty_terminal`, which sees the same bytes.
use std::path::PathBuf;

/// Where the shell is, from OSC 133.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptState {
    /// `A`: the prompt is being drawn; the shell waits for input.
    AtPrompt,
    /// `C`: a command line was accepted and is running.
    CommandRunning,
    /// `D;<code>`: the command ended; `None` when the shell gave no numeric code.
    CommandDone(Option<i32>),
}

/// A sequence hydraui acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc {
    Cwd(PathBuf),
    Prompt(PromptState),
}

/// Longest OSC payload kept. Longer ones (a huge title, an inline image) are skipped.
const MAX_PAYLOAD: usize = 4096;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After ESC.
    Esc,
    /// Inside `ESC ]`, collecting the payload.
    Osc,
    /// ESC inside an OSC: `\` ends it (ST), `]` starts a new one, anything else abandons it.
    OscEsc,
}

/// Scans PTY output for OSC sequences, keeping its state between reads so a sequence split
/// across two reads is still found. Only 7-bit introducers and terminators are recognised:
/// the C1 bytes 0x9c/0x9d also occur inside UTF-8 text.
#[derive(Debug, Default)]
pub struct OscScanner {
    state: State,
    payload: Vec<u8>,
}

impl OscScanner {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Osc> {
        let mut found = Vec::new();
        for &b in bytes {
            self.state = match (self.state, b) {
                (State::Ground, 0x1b) => State::Esc,
                (State::Ground, _) => State::Ground,
                (State::Esc, b']') => {
                    self.payload.clear();
                    State::Osc
                }
                (State::Esc, 0x1b) => State::Esc,
                (State::Esc, _) => State::Ground,
                (State::Osc, 0x07) => {
                    found.extend(interpret(&self.payload));
                    State::Ground
                }
                (State::Osc, 0x1b) => State::OscEsc,
                (State::Osc, _) if self.payload.len() >= MAX_PAYLOAD => State::Ground,
                (State::Osc, _) => {
                    self.payload.push(b);
                    State::Osc
                }
                (State::OscEsc, b'\\') => {
                    found.extend(interpret(&self.payload));
                    State::Ground
                }
                (State::OscEsc, b']') => {
                    self.payload.clear();
                    State::Osc
                }
                (State::OscEsc, _) => State::Ground,
            };
        }
        found
    }
}

/// What a complete OSC payload means to hydraui, if anything.
fn interpret(payload: &[u8]) -> Option<Osc> {
    let text = std::str::from_utf8(payload).ok()?;
    let (code, rest) = text.split_once(';')?;
    match code {
        "7" => parse_osc7(rest).map(Osc::Cwd),
        "133" => {
            let mut parts = rest.split(';');
            let state = match parts.next()? {
                "A" => PromptState::AtPrompt,
                "C" => PromptState::CommandRunning,
                "D" => PromptState::CommandDone(parts.next().and_then(|c| c.parse().ok())),
                _ => return None,
            };
            Some(Osc::Prompt(state))
        }
        _ => None,
    }
}

/// The folder in an OSC 7 `file://<host>/<path>` URL, as a Windows path; the host is
/// ignored. hydraui's own scripts don't percent-encode, so the path is taken as written.
/// `file://h/C:/x` → `C:\x`; `file://h///server/share` → `\\server\share`; else `None`.
pub fn parse_osc7(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')? + 1..];
    let b = path.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let unc = path.len() > 2 && path.starts_with("//");
    if !drive && !unc {
        return None;
    }
    Some(PathBuf::from(path.replace('/', "\\")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What hydraui's pwsh prompt prints after `cmd /c exit 3` in `C:\work\it's here`.
    const PROMPT: &[u8] =
        b"\x1b]133;D;3\x07\x1b]7;file://PC/C:/work/it's here\x07\x1b]133;A\x07PS> \x1b]133;B\x07";

    fn expected() -> Vec<Osc> {
        vec![
            Osc::Prompt(PromptState::CommandDone(Some(3))),
            Osc::Cwd(PathBuf::from("C:\\work\\it's here")),
            Osc::Prompt(PromptState::AtPrompt),
        ]
    }

    #[test]
    fn finds_a_prompts_marks_in_order() {
        assert_eq!(OscScanner::default().feed(PROMPT), expected());
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_found() {
        for cut in 0..=PROMPT.len() {
            let mut s = OscScanner::default();
            let mut got = s.feed(&PROMPT[..cut]);
            got.extend(s.feed(&PROMPT[cut..]));
            assert_eq!(got, expected(), "cut at {cut}");
        }
    }

    #[test]
    fn one_byte_at_a_time_works_too() {
        let mut s = OscScanner::default();
        let got: Vec<Osc> = PROMPT
            .iter()
            .flat_map(|b| s.feed(std::slice::from_ref(b)))
            .collect();
        assert_eq!(got, expected());
    }

    #[test]
    fn string_terminator_ends_a_sequence_like_bel() {
        let got = OscScanner::default().feed(b"\x1b]133;C\x1b\\");
        assert_eq!(got, vec![Osc::Prompt(PromptState::CommandRunning)]);
    }

    #[test]
    fn exit_codes_and_extra_parameters() {
        let got = OscScanner::default()
            .feed(b"\x1b]133;D\x07\x1b]133;D;oops\x07\x1b]133;D;-1\x07\x1b]133;A;cl=m\x07");
        assert_eq!(
            got,
            vec![
                Osc::Prompt(PromptState::CommandDone(None)),
                Osc::Prompt(PromptState::CommandDone(None)),
                Osc::Prompt(PromptState::CommandDone(Some(-1))),
                Osc::Prompt(PromptState::AtPrompt),
            ]
        );
    }

    #[test]
    fn other_sequences_and_text_are_ignored() {
        let got = OscScanner::default().feed(
            b"plain \x1b[31mred\x1b[0m \x1b]0;title\x07 \x1b]133;B\x07 \x1b]8;;http://x\x07link\x1b]8;;\x07",
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn malformed_input_is_dropped_and_scanning_recovers() {
        let mut input = Vec::new();
        // ESC followed by something other than `\` abandons the sequence.
        input.extend_from_slice(b"\x1b]133;A\x1bX");
        // Not UTF-8.
        input.extend_from_slice(b"\x1b]7;file://h/C:/\xff\x07");
        // Too long.
        input.extend_from_slice(b"\x1b]7;");
        input.extend(std::iter::repeat_n(b'a', MAX_PAYLOAD + 10));
        input.extend_from_slice(b"\x07");
        input.extend_from_slice(b"\x1b]133;C\x07");
        assert_eq!(
            OscScanner::default().feed(&input),
            vec![Osc::Prompt(PromptState::CommandRunning)]
        );
    }

    #[test]
    fn osc7_paths() {
        assert_eq!(
            parse_osc7("file://PC/C:/Users/me"),
            Some(PathBuf::from("C:\\Users\\me"))
        );
        assert_eq!(parse_osc7("file:///D:/x y"), Some(PathBuf::from("D:\\x y")));
        assert_eq!(parse_osc7("file://PC/C:/"), Some(PathBuf::from("C:\\")));
        // pwsh in \\server\share\dir prints file://PC///server/share/dir.
        assert_eq!(
            parse_osc7("file://PC///server/share/dir"),
            Some(PathBuf::from("\\\\server\\share\\dir"))
        );
        assert_eq!(parse_osc7("file://PC/home/me"), None);
        assert_eq!(parse_osc7("http://PC/C:/x"), None);
        assert_eq!(parse_osc7("file://PC"), None);
    }
}
