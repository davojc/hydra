fn main() {
    // VERSION in main.rs comes from HYDRA_VERSION at compile time; rebuild when it changes.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");

    // Embed the icon in hydra.exe (assets/make_icon.py draws it).
    println!("cargo:rerun-if-changed=../../assets/hydra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/hydra.ico");
        res.compile().expect("couldn't embed the hydra icon");
    }
}
