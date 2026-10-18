//! What installing changes outside the install folder (the user PATH, the Settings → Apps
//! entry, shortcuts, running `hydra init`), behind a trait so tests use a fake.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

/// The installer's Settings → Apps entry, under `HKCU`.
pub const SETTINGS_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\hydra";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsEntry {
    pub version: String,
    pub install_location: PathBuf,
    pub size_kb: u32,
}

pub trait System {
    fn user_path(&self) -> Option<String>;
    /// `None` deletes the value.
    fn set_user_path(&self, value: Option<&str>) -> anyhow::Result<()>;
    /// Tells running programs (Explorer, new terminals) the environment changed.
    fn environment_changed(&self);
    fn set_settings_entry(&self, entry: &SettingsEntry) -> anyhow::Result<()>;
    fn remove_settings_entry(&self);
    fn settings_entry(&self) -> Option<SettingsEntry>;
    fn create_shortcut(&self, lnk: &Path, target: &Path) -> anyhow::Result<()>;
    /// Runs `exe` with no window and waits; an error when it fails.
    fn run_hidden(&self, exe: &Path, args: &[&str]) -> anyhow::Result<()>;
}

/// An in-memory System for tests: it records what it's asked to do. Shortcuts also write an
/// empty file, so checks on the folder see them.
#[derive(Default)]
pub struct Fake {
    pub path: RefCell<Option<String>>,
    pub entry: RefCell<Option<SettingsEntry>>,
    pub shortcuts: RefCell<Vec<(PathBuf, PathBuf)>>,
    pub runs: RefCell<Vec<(PathBuf, Vec<String>)>>,
    pub changed: Cell<u32>,
}

impl System for Fake {
    fn user_path(&self) -> Option<String> {
        self.path.borrow().clone()
    }

    fn set_user_path(&self, value: Option<&str>) -> anyhow::Result<()> {
        *self.path.borrow_mut() = value.map(str::to_string);
        Ok(())
    }

    fn environment_changed(&self) {
        self.changed.set(self.changed.get() + 1);
    }

    fn set_settings_entry(&self, entry: &SettingsEntry) -> anyhow::Result<()> {
        *self.entry.borrow_mut() = Some(entry.clone());
        Ok(())
    }

    fn remove_settings_entry(&self) {
        *self.entry.borrow_mut() = None;
    }

    fn settings_entry(&self) -> Option<SettingsEntry> {
        self.entry.borrow().clone()
    }

    fn create_shortcut(&self, lnk: &Path, target: &Path) -> anyhow::Result<()> {
        if let Some(dir) = lnk.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(lnk, b"")?;
        self.shortcuts
            .borrow_mut()
            .push((lnk.to_path_buf(), target.to_path_buf()));
        Ok(())
    }

    fn run_hidden(&self, exe: &Path, args: &[&str]) -> anyhow::Result<()> {
        self.runs.borrow_mut().push((
            exe.to_path_buf(),
            args.iter().map(|a| a.to_string()).collect(),
        ));
        Ok(())
    }
}

/// The real thing: the registry, Windows shortcuts, hidden processes.
pub struct Real;

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::path::Path;

    use anyhow::Context;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE,
        REG_SZ, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
        RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    use windows::core::HSTRING;

    pub fn get_string(sub: &str, name: &str) -> Option<String> {
        let (sub, name) = (HSTRING::from(sub), HSTRING::from(name));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut size = 0u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &sub,
                &name,
                flags,
                None,
                None,
                Some(&mut size),
            )
        }
        .ok()
        .ok()?;
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size = (buf.len() * 2) as u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &sub,
                &name,
                flags,
                None,
                Some(buf.as_mut_ptr() as *mut c_void),
                Some(&mut size),
            )
        }
        .ok()
        .ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    fn ensure_key(sub: &str) -> anyhow::Result<()> {
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(sub),
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        }
        .ok()
        .with_context(|| format!("can't open HKCU\\{sub}"))?;
        let _ = unsafe { RegCloseKey(key) };
        Ok(())
    }

    pub fn set_string(sub: &str, name: &str, value: &str, expand: bool) -> anyhow::Result<()> {
        ensure_key(sub)?;
        let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let kind = if expand { REG_EXPAND_SZ } else { REG_SZ };
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(sub),
                &HSTRING::from(name),
                kind.0,
                Some(data.as_ptr() as *const c_void),
                (data.len() * 2) as u32,
            )
        }
        .ok()
        .with_context(|| format!("can't write HKCU\\{sub}\\{name}"))
    }

    pub fn set_dword(sub: &str, name: &str, value: u32) -> anyhow::Result<()> {
        ensure_key(sub)?;
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(sub),
                &HSTRING::from(name),
                REG_DWORD.0,
                Some(&value as *const u32 as *const c_void),
                4,
            )
        }
        .ok()
        .with_context(|| format!("can't write HKCU\\{sub}\\{name}"))
    }

    pub fn delete_value(sub: &str, name: &str) {
        let _ = unsafe {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(sub), &HSTRING::from(name))
        };
    }

    pub fn delete_tree(sub: &str) {
        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(sub)) };
    }

    pub fn broadcast_environment() {
        let name: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(name.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                2000,
                None,
            );
        }
    }

    pub fn shortcut(lnk: &Path, target: &Path) -> anyhow::Result<()> {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::Interface;
        if let Some(dir) = lnk.parent() {
            std::fs::create_dir_all(dir)?;
        }
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(dir) = target.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            link.cast::<IPersistFile>()?
                .Save(&HSTRING::from(lnk.as_os_str()), true)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
impl System for Real {
    fn user_path(&self) -> Option<String> {
        win::get_string("Environment", "Path")
    }

    fn set_user_path(&self, value: Option<&str>) -> anyhow::Result<()> {
        match value {
            Some(v) => win::set_string("Environment", "Path", v, true),
            None => {
                win::delete_value("Environment", "Path");
                Ok(())
            }
        }
    }

    fn environment_changed(&self) {
        win::broadcast_environment();
    }

    fn set_settings_entry(&self, e: &SettingsEntry) -> anyhow::Result<()> {
        let dir = e.install_location.display().to_string();
        let setup = format!("\"{dir}\\hydra-setup.exe\"");
        let k = SETTINGS_SUBKEY;
        win::set_string(k, "DisplayName", "hydra", false)?;
        win::set_string(k, "DisplayVersion", &e.version, false)?;
        win::set_string(k, "Publisher", "davojc", false)?;
        win::set_string(k, "DisplayIcon", &format!("{dir}\\hydraui.exe"), false)?;
        win::set_string(k, "InstallLocation", &dir, false)?;
        win::set_string(k, "UninstallString", &format!("{setup} --uninstall"), false)?;
        win::set_string(
            k,
            "QuietUninstallString",
            &format!("{setup} --uninstall --silent"),
            false,
        )?;
        win::set_string(k, "URLInfoAbout", "https://github.com/davojc/hydra", false)?;
        win::set_dword(k, "NoModify", 1)?;
        win::set_dword(k, "NoRepair", 1)?;
        win::set_dword(k, "EstimatedSize", e.size_kb)
    }

    fn remove_settings_entry(&self) {
        win::delete_tree(SETTINGS_SUBKEY);
    }

    fn settings_entry(&self) -> Option<SettingsEntry> {
        Some(SettingsEntry {
            version: win::get_string(SETTINGS_SUBKEY, "DisplayVersion")?,
            install_location: win::get_string(SETTINGS_SUBKEY, "InstallLocation")?.into(),
            size_kb: 0,
        })
    }

    fn create_shortcut(&self, lnk: &Path, target: &Path) -> anyhow::Result<()> {
        win::shortcut(lnk, target)
    }

    fn run_hidden(&self, exe: &Path, args: &[&str]) -> anyhow::Result<()> {
        use std::os::windows::process::CommandExt;
        let status = std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .status()?;
        anyhow::ensure!(status.success(), "{} exited with {status}", exe.display());
        Ok(())
    }
}
