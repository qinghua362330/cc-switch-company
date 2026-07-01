fn main() {
    tauri_build::build();

    // Windows: Embed Common Controls v6 manifest for binaries and tests.
    //
    // `TaskDialogIndirect` is exported by the Common Controls v6 activation
    // context. If the portable exe is shipped without this manifest, older
    // Windows setups can fail at process startup before our Rust code runs.
    #[cfg(target_os = "windows")]
    {
        let manifest_path = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"),
        )
        .join("common-controls.manifest");
        let manifest_arg = format!("/MANIFESTINPUT:{}", manifest_path.display());

        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg={}", manifest_arg);
        println!("cargo:rerun-if-changed={}", manifest_path.display());
    }
}
