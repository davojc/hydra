fn main() {
    // VERSION in main.rs comes from HYDRA_VERSION at compile time; rebuild when it changes.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");

    // Embed the icon (assets/make_icon.py draws it) and version info in hydra.exe.
    // winresource sets FileVersion and ProductVersion from CARGO_PKG_VERSION.
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
        res.compile()
            .expect("couldn't embed the hydra icon and version info");
    }
}
