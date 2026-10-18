//! hydra-setup's dialogs (spec §7.7): Windows task dialogs for confirming, showing progress
//! and saying how it went.
use std::path::Path;
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, S_FALSE, S_OK, WPARAM};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_FLAGS, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TD_ERROR_ICON,
    TD_INFORMATION_ICON, TDCBF_CANCEL_BUTTON, TDCBF_CLOSE_BUTTON, TDE_CONTENT,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER, TDF_SHOW_PROGRESS_BAR,
    TDF_VERIFICATION_FLAG_CHECKED, TDM_CLICK_BUTTON, TDM_ENABLE_BUTTON, TDM_SET_ELEMENT_TEXT,
    TDM_SET_PROGRESS_BAR_POS, TDM_SET_PROGRESS_BAR_RANGE, TDN_BUTTON_CLICKED, TDN_CREATED,
    TDN_TIMER, TaskDialogIndirect,
};
use windows::Win32::UI::WindowsAndMessaging::{IDCANCEL, SendMessageW};
use windows::core::{BOOL, HRESULT, PCWSTR};

use crate::install::{Report, Step};

const TITLE: &str = "hydra setup";
const INSTALL: i32 = 100;
const UNINSTALL: i32 = 101;

/// A NUL-terminated UTF-16 copy of `s`, kept alive while the dialog uses it.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn pcwstr(w: &[u16]) -> PCWSTR {
    PCWSTR(w.as_ptr())
}

/// One dialog's text and choices.
struct Page<'a> {
    instruction: &'a str,
    content: &'a str,
    icon: PCWSTR,
    buttons: &'a [(i32, &'a str)],
    close: bool,
    verification: Option<(&'a str, bool)>,
}

/// Shows `page`; the button pressed and whether the tick box ended up ticked.
fn show(page: &Page) -> (i32, bool) {
    let (title, instruction, content) = (wide(TITLE), wide(page.instruction), wide(page.content));
    let labels: Vec<Vec<u16>> = page.buttons.iter().map(|(_, l)| wide(l)).collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = page
        .buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), l)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: pcwstr(l),
        })
        .collect();
    let verification = page.verification.map(|(t, _)| wide(t));
    let mut flags = TDF_ALLOW_DIALOG_CANCELLATION;
    if page.verification.is_some_and(|(_, on)| on) {
        flags |= TDF_VERIFICATION_FLAG_CHECKED;
    }
    let mut cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: flags,
        dwCommonButtons: if page.close {
            TDCBF_CLOSE_BUTTON
        } else {
            TDCBF_CANCEL_BUTTON
        },
        pszWindowTitle: pcwstr(&title),
        pszMainInstruction: pcwstr(&instruction),
        pszContent: pcwstr(&content),
        cButtons: buttons.len() as u32,
        pButtons: if buttons.is_empty() {
            std::ptr::null()
        } else {
            buttons.as_ptr()
        },
        nDefaultButton: page.buttons.first().map_or(0, |(id, _)| *id),
        ..Default::default()
    };
    cfg.Anonymous1.pszMainIcon = page.icon;
    if let Some(v) = &verification {
        cfg.pszVerificationText = pcwstr(v);
    }
    let (mut pressed, mut ticked) = (0i32, BOOL(0));
    let shown = unsafe { TaskDialogIndirect(&cfg, Some(&mut pressed), None, Some(&mut ticked)) };
    if shown.is_err() {
        return (IDCANCEL.0, false);
    }
    (pressed, ticked.as_bool())
}

/// "Install hydra <version>?": `Some(desktop shortcut wanted)`, or `None` when cancelled.
pub fn confirm_install(version: &str, dir: &Path) -> Option<bool> {
    let content = format!(
        "hydraui and hydra, for you only, in {}.\n\nhydraui goes in the Start menu and hydra on your PATH.",
        dir.display()
    );
    let (pressed, desktop) = show(&Page {
        instruction: &format!("Install hydra {version}"),
        content: &content,
        icon: TD_INFORMATION_ICON,
        buttons: &[(INSTALL, "Install")],
        close: false,
        verification: Some(("Create a desktop shortcut", false)),
    });
    (pressed == INSTALL).then_some(desktop)
}

/// "Installed": whether "Launch hydraui" was left ticked.
pub fn done(report: &Report) -> bool {
    let mut content = String::from("Open a new terminal to use hydra.");
    if let Some(dir) = &report.other_hydra {
        content.push_str(&format!(
            "\n\nAnother hydra.exe is on your PATH, in {}. Windows may run that one; this setup left it alone.",
            dir.display()
        ));
    }
    if let Some(why) = &report.init_failed {
        content.push_str(&format!(
            "\n\n\"hydra init\" didn't finish ({why}); run it yourself."
        ));
    }
    show(&Page {
        instruction: "hydra is installed",
        content: &content,
        icon: TD_INFORMATION_ICON,
        buttons: &[],
        close: true,
        verification: Some(("Launch hydraui", true)),
    })
    .1
}

pub fn failed(instruction: &str, message: &str) {
    show(&Page {
        instruction,
        content: message,
        icon: TD_ERROR_ICON,
        buttons: &[],
        close: true,
        verification: None,
    });
}

pub fn confirm_uninstall() -> bool {
    show(&Page {
        instruction: "Uninstall hydra?",
        content: "This removes hydraui and hydra, their shortcuts and the PATH entry. Your environments in ~/.hydra stay.",
        icon: TD_INFORMATION_ICON,
        buttons: &[(UNINSTALL, "Uninstall")],
        close: false,
        verification: None,
    })
    .0 == UNINSTALL
}

pub fn uninstalled() {
    show(&Page {
        instruction: "hydra was removed",
        content: "Your environments in ~/.hydra are still there.",
        icon: TD_INFORMATION_ICON,
        buttons: &[],
        close: true,
        verification: None,
    });
}

/// What the work thread reports to the progress dialog.
#[derive(Default)]
struct Progress {
    /// 0–1000.
    pos: u32,
    text: String,
    result: Option<Result<Report, String>>,
}

/// The progress bar's position for a step: hydraui is about 70% of the download.
fn position(step: Step) -> (u32, String) {
    match step {
        Step::Downloading { asset, done, total } => {
            let ui = asset.starts_with("hydraui");
            let (start, span) = if ui { (0.0, 700.0) } else { (700.0, 290.0) };
            let frac = total.map_or(0.0, |t| done as f64 / t.max(1) as f64);
            let name = if ui { "hydraui" } else { "hydra" };
            let size = match total {
                Some(t) => format!(" ({:.0} of {:.0} MB)", done as f64 / 1e6, t as f64 / 1e6),
                None => String::new(),
            };
            (
                (start + span * frac) as u32,
                format!("Downloading {name}{size}…"),
            )
        }
        Step::Installing => (990, "Installing…".to_string()),
    }
}

unsafe extern "system" fn progress_callback(
    hwnd: HWND,
    msg: TASKDIALOG_NOTIFICATIONS,
    _: WPARAM,
    _: LPARAM,
    data: isize,
) -> HRESULT {
    let state = unsafe { &*(data as *const Mutex<Progress>) };
    let send = |m: i32, w: usize, l: isize| unsafe {
        SendMessageW(hwnd, m as u32, Some(WPARAM(w)), Some(LPARAM(l)));
    };
    match msg {
        TDN_CREATED => {
            send(TDM_SET_PROGRESS_BAR_RANGE.0, 0, (1000 << 16) as isize);
            send(TDM_ENABLE_BUTTON.0, IDCANCEL.0 as usize, 0);
            S_OK
        }
        TDN_TIMER => {
            // Copy out and let go of the lock first: clicking the button below calls back
            // into this function on the same thread (TDN_BUTTON_CLICKED), which locks again.
            let (pos, text, finished) = {
                let s = state.lock().unwrap();
                (s.pos, wide(&s.text), s.result.is_some())
            };
            send(TDM_SET_PROGRESS_BAR_POS.0, pos as usize, 0);
            send(
                TDM_SET_ELEMENT_TEXT.0,
                TDE_CONTENT.0 as usize,
                text.as_ptr() as isize,
            );
            if finished {
                send(TDM_CLICK_BUTTON.0, IDCANCEL.0 as usize, 0);
            }
            S_OK
        }
        // Only the finished work closes the dialog.
        TDN_BUTTON_CLICKED => {
            if state.lock().unwrap().result.is_some() {
                S_OK
            } else {
                S_FALSE
            }
        }
        _ => S_OK,
    }
}

/// Runs `work` on a thread behind a progress dialog, and returns its result.
pub fn with_progress<W>(version: &str, work: W) -> Result<Report, String>
where
    W: FnOnce(&dyn Fn(Step)) -> anyhow::Result<Report> + Send + 'static,
{
    let state = Arc::new(Mutex::new(Progress {
        text: "Starting…".into(),
        ..Default::default()
    }));
    let worker = state.clone();
    std::thread::spawn(move || {
        let report = |step: Step| {
            let (pos, text) = position(step);
            let mut s = worker.lock().unwrap();
            s.pos = pos;
            s.text = text;
        };
        let result = work(&report).map_err(|e| format!("{e:#}"));
        worker.lock().unwrap().result = Some(result);
    });
    let (title, instruction, starting) = (
        wide(TITLE),
        wide(&format!("Installing hydra {version}")),
        wide("Starting…"),
    );
    let cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER | TASKDIALOG_FLAGS(0),
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: pcwstr(&title),
        pszMainInstruction: pcwstr(&instruction),
        pszContent: pcwstr(&starting),
        pfCallback: Some(progress_callback),
        lpCallbackData: Arc::as_ptr(&state) as isize,
        ..Default::default()
    };
    let _ = unsafe { TaskDialogIndirect(&cfg, None, None, None) };
    // The dialog only closes once the work has finished, but wait in case it was dismissed.
    loop {
        if let Some(r) = state.lock().unwrap().result.take() {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
