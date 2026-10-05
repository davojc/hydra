fn main() {
    // VERSION in main.rs comes from HYDRA_VERSION at compile time; rebuild when it changes.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");

    // Embed the icon (assets/make_icon.py draws it) and version info in hydra.exe.
    println!("cargo:rerun-if-changed=../../assets/hydra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/hydra.ico")
            .set(
                "FileDescription",
                "hydra - terminals with separate identities per environment",
            )
            .set("ProductName", "hydra")
            .set("CompanyName", "davojc")
            .set("OriginalFilename", "hydra.exe")
            .set("InternalName", "hydra")
            .set("LegalCopyright", "Copyright (c) davojc");
        // A release's version (HYDRA_VERSION, from the git tag) when it is a plain x.y.z;
        // otherwise winresource's default, the Cargo.toml version.
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
            .expect("couldn't embed the hydra icon and version info");
    }
}

/// `x.y.z` (digits only, each part fitting 16 bits) packed as `x << 48 | y << 32 | z << 16`.
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
