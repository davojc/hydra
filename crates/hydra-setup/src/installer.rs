//! The Windows installer's entry point: installs hydraui and hydra for the current user,
//! and uninstalls them (spec §7.7).
use std::path::{Path, PathBuf};

use hydra_setup::install::{self, Plan, Source};
use hydra_setup::system::Real;
use hydra_setup::{VERSION, args, dialog};

pub fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let code = match args::parse(&raw) {
        Ok(a) if a.uninstall => uninstall(&a),
        Ok(a) => run_install(&a),
        Err(e) => {
            if !raw.iter().any(|a| a == "--silent") {
                dialog::failed("hydra setup can't start", &e);
            }
            2
        }
    };
    std::process::exit(code);
}

fn plan_for(a: &args::Args) -> Result<Plan, String> {
    let source = match (&a.from, VERSION.ends_with("-dev")) {
        (Some(dir), _) => Source::From(dir.clone()),
        (None, true) => {
            return Err(
                "This is a development build of hydra setup; it can only install --from a folder."
                    .into(),
            );
        }
        (None, false) => Source::Download {
            base: std::env::var("HYDRA_SETUP_BASE")
                .unwrap_or_else(|_| "https://github.com/davojc/hydra/releases/download".into()),
        },
    };
    let mut plan = install::default_plan(VERSION, source, a.desktop);
    if let Some(dir) = &a.dir {
        plan.dir = dir.clone();
    }
    Ok(plan)
}

fn run_install(a: &args::Args) -> i32 {
    let mut plan = match plan_for(a) {
        Ok(p) => p,
        Err(e) => {
            if !a.silent {
                dialog::failed("hydra wasn't installed", &e);
            }
            return 2;
        }
    };
    if a.silent {
        return match install::install(&plan, &Real, &|_| {}) {
            Ok(_) => 0,
            Err(_) => 1,
        };
    }
    let Some(desktop) = dialog::confirm_install(VERSION, &plan.dir) else {
        return 1;
    };
    plan.desktop = desktop;
    let dir = plan.dir.clone();
    let worked = dialog::with_progress(VERSION, move |progress| {
        install::install(&plan, &Real, progress)
    });
    match worked {
        Ok(report) => {
            if dialog::done(&report) {
                let _ = std::process::Command::new(dir.join("hydraui.exe"))
                    .current_dir(&dir)
                    .spawn();
            }
            0
        }
        Err(e) => {
            dialog::failed("hydra wasn't installed", &e);
            1
        }
    }
}

/// The folder the uninstaller runs from: `--dir`, else where this exe is.
fn install_dir(a: &args::Args) -> PathBuf {
    a.dir.clone().unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
            .unwrap_or_default()
    })
}

fn uninstall(a: &args::Args) -> i32 {
    use hydra_setup::system::System;
    let dir = match install::uninstall_target(
        a.dir.as_deref(),
        &install_dir(&args::Args::default()),
        Real.settings_entry().as_ref(),
    ) {
        Ok(d) => d,
        Err(e) => {
            if !a.silent {
                dialog::failed("hydra wasn't removed", &e);
            }
            return 1;
        }
    };
    if !a.silent && !dialog::confirm_uninstall() {
        return 1;
    }
    let plan = install::default_plan(VERSION, Source::From(PathBuf::new()), false);
    let result = install::uninstall(&dir, &plan.start_menu, &plan.desktop_dir, &Real);
    if let Err(e) = &result {
        if !a.silent {
            dialog::failed("hydra wasn't fully removed", &format!("{e:#}"));
        }
        return 1;
    }
    if !a.silent {
        dialog::uninstalled();
    }
    delete_self_later(&dir);
    0
}

/// A running exe can't delete itself: a hidden `cmd` waits for it to exit, then removes it
/// and the folder (if empty).
fn delete_self_later(dir: &Path) {
    use std::os::windows::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let script = format!(
        "ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
        exe.display(),
        dir.display()
    );
    let _ = std::process::Command::new("cmd")
        .args(["/d", "/c"])
        .raw_arg(script)
        .creation_flags(0x0800_0000)
        .spawn();
}
