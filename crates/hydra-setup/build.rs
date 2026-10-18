/// Runs as the user, never elevated (an exe named "setup" would otherwise trigger Windows'
/// installer detection and an admin prompt); common controls 6 for the task dialogs; DPI aware.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="davojc.hydra.setup" version="1.0.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;

fn main() {
    // hydra-setup's version follows the release (HYDRA_VERSION) like hydra.exe's.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");
    println!("cargo:rerun-if-changed=../../assets/hydra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/hydra.ico")
            .set("FileDescription", "hydra setup")
            .set("ProductName", "hydra")
            .set("CompanyName", "davojc")
            .set("OriginalFilename", "hydra-setup.exe")
            .set("InternalName", "hydra-setup")
            .set("LegalCopyright", "Copyright (c) davojc")
            .set_manifest(MANIFEST);
        if let Some((v, packed)) = std::env::var("HYDRA_VERSION")
            .ok()
            .and_then(|v| release_version(&v).map(|p| (v, p)))
        {
            res.set("FileVersion", &v)
                .set("ProductVersion", &v)
                .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
                .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
        }
        res.compile()
            .expect("couldn't embed the hydra-setup icon, version and manifest");
    }
}

fn release_version(v: &str) -> Option<u64> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut packed = 0_u64;
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u16 = p.parse().ok()?;
        packed |= u64::from(n) << (48 - 16 * i);
    }
    Some(packed)
}
