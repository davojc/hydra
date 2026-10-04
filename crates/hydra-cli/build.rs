fn main() {
    // VERSION in main.rs comes from HYDRA_VERSION at compile time; rebuild when it changes.
    println!("cargo:rerun-if-env-changed=HYDRA_VERSION");
}
