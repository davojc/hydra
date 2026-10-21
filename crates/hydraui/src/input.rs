//! What keys, pastes and mouse events send to the shell (xterm encodings).
use hydraui_term::alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// The bytes for a key press, or `None` for plain text: that arrives through the window's
/// input handler instead, so IME and AltGr characters work. `key` is GPUI's key name.
pub fn key_bytes(key: &str, key_char: Option<&str>, m: Mods, mode: TermMode) -> Option<Vec<u8>> {
    let param = m.param();
    let csi = |s: String| format!("\x1b[{s}").into_bytes();
    let out = match key {
        "enter" if m.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.ctrl => b"\x08".to_vec(),
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "tab" if m.shift => csi("Z".into()),
        "tab" => b"\t".to_vec(),
        "up" | "down" | "right" | "left" | "home" | "end" => {
            let c = match key {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                "left" => 'D',
                "home" => 'H',
                _ => 'F',
            };
            if param > 1 {
                csi(format!("1;{param}{c}"))
            } else if mode.contains(TermMode::APP_CURSOR) {
                format!("\x1bO{c}").into_bytes()
            } else {
                csi(c.to_string())
            }
        }
        "insert" | "delete" | "pageup" | "pagedown" => {
            let n = match key {
                "insert" => 2,
                "delete" => 3,
                "pageup" => 5,
                _ => 6,
            };
            if param > 1 {
                csi(format!("{n};{param}~"))
            } else {
                csi(format!("{n}~"))
            }
        }
        "space" if m.ctrl => vec![0],
        _ => {
            if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                return function_key(n, param);
            }
            if m.ctrl && !m.alt {
                return control(key);
            }
            if m.alt && !m.ctrl {
                let mut out = vec![0x1b];
                out.extend_from_slice(key_char.unwrap_or(key).as_bytes());
                return Some(out);
            }
            // Plain text, or Ctrl+Alt (AltGr on many layouts): the input handler's.
            return None;
        }
    };
    Some(out)
}

fn function_key(n: u8, param: u8) -> Option<Vec<u8>> {
    let s = match n {
        1..=4 => {
            let c = b"PQRS"[usize::from(n - 1)] as char;
            if param > 1 {
                format!("\x1b[1;{param}{c}")
            } else {
                format!("\x1bO{c}")
            }
        }
        5..=12 => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            if param > 1 {
                format!("\x1b[{code};{param}~")
            } else {
                format!("\x1b[{code}~")
            }
        }
        _ => return None,
    };
    Some(s.into_bytes())
}

/// Ctrl+key: the C0 control code.
fn control(key: &str) -> Option<Vec<u8>> {
    let &[b] = key.as_bytes() else {
        return None;
    };
    let code = match b {
        b'a'..=b'z' => b - b'a' + 1,
        b'@' | b'2' => 0,
        b'[' | b'3' => 0x1b,
        b'\\' | b'4' => 0x1c,
        b']' | b'5' => 0x1d,
        b'^' | b'6' => 0x1e,
        b'_' | b'-' | b'7' => 0x1f,
        b'8' => 0x7f,
        _ => return None,
    };
    Some(vec![code])
}

/// Whether Ctrl+V pastes the clipboard's text, as in Windows Terminal: only plain Ctrl+V, and
/// only when there is text (`has_text`). Without text, ^V reaches the program, so Claude Code
/// can paste an image. Never on macOS, where Cmd+V pastes and ^V is always the program's.
pub fn ctrl_v_pastes(key: &str, m: Mods, has_text: bool) -> bool {
    !cfg!(target_os = "macos") && key == "v" && m.ctrl && !m.alt && !m.shift && has_text
}

/// Pasted text for the shell: every line ending becomes one Enter (`\r`). With bracketed
/// paste on, the text is wrapped and ESC is removed so it can't end the bracket early.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let clean: String = text.chars().filter(|c| *c != '\x1b').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Move,
}

/// The report for a mouse event at a 0-based cell when the program asked for mouse
/// reporting; `None` means the event is hydraui's own (selection, scrollback).
pub fn mouse_report(
    button: Option<ReportButton>,
    kind: MouseKind,
    col: usize,
    row: usize,
    m: Mods,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let wanted = match kind {
        MouseKind::Press | MouseKind::Release => mode.intersects(TermMode::MOUSE_MODE),
        MouseKind::Move if button.is_some() => {
            mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        }
        MouseKind::Move => mode.contains(TermMode::MOUSE_MOTION),
    };
    if !wanted {
        return None;
    }
    let mut code: u32 = match button {
        Some(ReportButton::Left) => 0,
        Some(ReportButton::Middle) => 1,
        Some(ReportButton::Right) => 2,
        Some(ReportButton::WheelUp) => 64,
        Some(ReportButton::WheelDown) => 65,
        None => 3,
    };
    if kind == MouseKind::Move {
        code += 32;
    }
    let mods = 4 * u32::from(m.shift) + 8 * u32::from(m.alt) + 16 * u32::from(m.ctrl);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if kind == MouseKind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{};{}{end}", code + mods, col + 1, row + 1).into_bytes());
    }
    // X10 encoding: a release doesn't say which button, and cells stop at 223.
    if kind == MouseKind::Release {
        code = 3;
    }
    let (x, y) = (col + 33, row + 33);
    if x > 255 || y > 255 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        (32 + code + mods) as u8,
        x as u8,
        y as u8,
    ])
}

/// What a wheel turn does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wheel {
    /// Bytes for the program: mouse reports, or arrow keys on the alternate screen.
    Send(Vec<u8>),
    /// Scroll hydraui's view of the history by this many lines (> 0 is up).
    Scroll(i32),
}

/// A wheel turn of `lines` (> 0 is up) over a 0-based cell.
pub fn wheel(lines: i32, col: usize, row: usize, m: Mods, mode: TermMode) -> Wheel {
    let n = lines.unsigned_abs() as usize;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button = if lines > 0 {
            ReportButton::WheelUp
        } else {
            ReportButton::WheelDown
        };
        if let Some(one) = mouse_report(Some(button), MouseKind::Press, col, row, m, mode) {
            return Wheel::Send(one.repeat(n));
        }
    }
    if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        let c = if lines > 0 { 'A' } else { 'B' };
        let one = if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        };
        return Wheel::Send(one.repeat(n).into_bytes());
    }
    Wheel::Scroll(lines)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ctrl_v_pastes_text_and_leaves_an_image_to_the_program() {
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        // macOS pastes with Cmd+V (the Paste command); Ctrl+V is the program's, as in Terminal.
        assert_eq!(ctrl_v_pastes("v", ctrl, true), !cfg!(target_os = "macos"));
        assert!(!ctrl_v_pastes("v", ctrl, false), "no text: ^V goes through");
        assert!(!ctrl_v_pastes("v", Mods::default(), true));
        let ctrl_shift = Mods {
            shift: true,
            ..ctrl
        };
        assert!(
            !ctrl_v_pastes("v", ctrl_shift, true),
            "Ctrl+Shift+V is the Paste command"
        );
        let ctrl_alt = Mods { alt: true, ..ctrl };
        assert!(
            !ctrl_v_pastes("v", ctrl_alt, true),
            "AltGr+V and Ctrl+Alt+V are left alone"
        );
        assert!(!ctrl_v_pastes("c", ctrl, true));
    }

    use super::*;

    const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
    };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };
    const SHIFT: Mods = Mods {
        shift: true,
        ..NONE
    };

    fn key(k: &str, ch: Option<&str>, m: Mods) -> Option<Vec<u8>> {
        key_bytes(k, ch, m, TermMode::empty())
    }

    fn b(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    #[test]
    fn plain_text_is_left_to_the_input_handler() {
        assert_eq!(key("a", Some("a"), NONE), None);
        assert_eq!(key("a", Some("A"), SHIFT), None);
        assert_eq!(key("space", Some(" "), NONE), None);
    }

    #[test]
    fn altgr_is_text_not_a_control_sequence() {
        let altgr = Mods {
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert_eq!(key("q", Some("@"), altgr), None);
    }

    #[test]
    fn special_keys() {
        assert_eq!(key("enter", None, NONE), b("\r"));
        assert_eq!(key("enter", None, ALT), b("\x1b\r"));
        assert_eq!(key("escape", None, NONE), b("\x1b"));
        assert_eq!(key("backspace", None, NONE), b("\x7f"));
        assert_eq!(key("backspace", None, CTRL), b("\x08"));
        assert_eq!(key("backspace", None, ALT), b("\x1b\x7f"));
        assert_eq!(key("tab", None, NONE), b("\t"));
        assert_eq!(key("tab", None, SHIFT), b("\x1b[Z"));
        assert_eq!(key("delete", None, NONE), b("\x1b[3~"));
        assert_eq!(key("pageup", None, NONE), b("\x1b[5~"));
    }

    #[test]
    fn arrows_follow_cursor_key_mode_and_modifiers() {
        assert_eq!(key("up", None, NONE), b("\x1b[A"));
        assert_eq!(
            key_bytes("up", None, NONE, TermMode::APP_CURSOR),
            b("\x1bOA")
        );
        assert_eq!(key("right", None, CTRL), b("\x1b[1;5C"));
        assert_eq!(key("home", None, SHIFT), b("\x1b[1;2H"));
        assert_eq!(
            key(
                "delete",
                None,
                Mods {
                    ctrl: true,
                    shift: true,
                    alt: false
                }
            ),
            b("\x1b[3;6~")
        );
    }

    #[test]
    fn function_keys() {
        assert_eq!(key("f1", None, NONE), b("\x1bOP"));
        assert_eq!(key("f5", None, NONE), b("\x1b[15~"));
        assert_eq!(key("f12", None, NONE), b("\x1b[24~"));
        assert_eq!(key("f1", None, CTRL), b("\x1b[1;5P"));
        assert_eq!(key("f5", None, SHIFT), b("\x1b[15;2~"));
        assert_eq!(key("f13", None, NONE), None);
        assert_eq!(key("f", Some("f"), NONE), None);
    }

    #[test]
    fn control_and_alt_combinations() {
        assert_eq!(key("c", None, CTRL), Some(vec![3]));
        assert_eq!(key("a", None, CTRL), Some(vec![1]));
        assert_eq!(key("z", None, CTRL), Some(vec![26]));
        assert_eq!(key("[", None, CTRL), Some(vec![0x1b]));
        assert_eq!(key("space", None, CTRL), Some(vec![0]));
        assert_eq!(key("x", Some("x"), ALT), b("\x1bx"));
        assert_eq!(
            key(
                "x",
                Some("X"),
                Mods {
                    alt: true,
                    shift: true,
                    ctrl: false
                }
            ),
            b("\x1bX")
        );
    }

    #[test]
    fn paste_turns_every_line_ending_into_one_return() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn bracketed_paste_wraps_and_cannot_be_escaped() {
        assert_eq!(
            paste_bytes("x\x1b[201~y\r\n", true),
            b"\x1b[200~x[201~y\r\x1b[201~".to_vec()
        );
    }

    #[test]
    fn mouse_reports_only_when_the_program_asks() {
        assert_eq!(
            mouse_report(
                Some(ReportButton::Left),
                MouseKind::Press,
                0,
                0,
                NONE,
                TermMode::empty()
            ),
            None
        );
        let click = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse_report(None, MouseKind::Move, 0, 0, NONE, click), None);
    }

    #[test]
    fn sgr_mouse_reports() {
        let m = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let r = |b, k, col, row, mods| mouse_report(b, k, col, row, mods, m);
        assert_eq!(
            r(Some(ReportButton::Left), MouseKind::Press, 4, 2, NONE),
            b("\x1b[<0;5;3M")
        );
        assert_eq!(
            r(Some(ReportButton::Left), MouseKind::Release, 4, 2, NONE),
            b("\x1b[<0;5;3m")
        );
        assert_eq!(
            r(Some(ReportButton::Right), MouseKind::Press, 0, 0, CTRL),
            b("\x1b[<18;1;1M")
        );
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Move, 0, 0, NONE, drag),
            b("\x1b[<32;1;1M")
        );
    }

    #[test]
    fn legacy_mouse_reports() {
        let m = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Press, 0, 0, NONE, m),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Release, 0, 0, NONE, m),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        assert_eq!(
            mouse_report(Some(ReportButton::Left), MouseKind::Press, 300, 0, NONE, m),
            None
        );
    }

    #[test]
    fn the_wheel_scrolls_reports_or_sends_arrows() {
        assert_eq!(wheel(2, 0, 0, NONE, TermMode::empty()), Wheel::Scroll(2));
        let alt = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(wheel(-1, 0, 0, NONE, alt), Wheel::Send(b"\x1b[B".to_vec()));
        assert_eq!(
            wheel(1, 0, 0, NONE, alt | TermMode::APP_CURSOR),
            Wheel::Send(b"\x1bOA".to_vec())
        );
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            wheel(-2, 0, 0, NONE, sgr),
            Wheel::Send(b"\x1b[<65;1;1M\x1b[<65;1;1M".to_vec())
        );
    }
}
