fn main() {
    // Grammars gpui-component doesn't ship (src/grammars.rs).
    cc::Build::new()
        .include("grammars/typespec/src")
        .file("grammars/typespec/src/parser.c")
        .warnings(false)
        .compile("tree-sitter-typespec");
    cc::Build::new()
        .include("grammars/dockerfile/src")
        .file("grammars/dockerfile/src/parser.c")
        .file("grammars/dockerfile/src/scanner.c")
        .warnings(false)
        .compile("tree-sitter-dockerfile");
    cc::Build::new()
        .include("grammars/hcl/src")
        .file("grammars/hcl/src/parser.c")
        .file("grammars/hcl/src/scanner.c")
        .warnings(false)
        .compile("tree-sitter-hcl");
    println!("cargo:rerun-if-changed=grammars");
    // hydraui's version info follows the release (HYDRA_VERSION) like hydra.exe's.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");
    println!("cargo:rerun-if-changed=../../assets/hydra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/hydra.ico")
            .set(
                "FileDescription",
                "hydraui - a desktop terminal for hydra environments",
            )
            .set("ProductName", "hydraui")
            .set("CompanyName", "davojc")
            .set("OriginalFilename", "hydraui.exe")
            .set("InternalName", "hydraui")
            .set("LegalCopyright", "Copyright (c) davojc");
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
            .expect("couldn't embed the hydraui icon and version info");
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
