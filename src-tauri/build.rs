fn main() {
    #[cfg(target_os = "windows")]
    {
        let windows = tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("common-controls.manifest"));
        let attrs = tauri_build::Attributes::new().windows_attributes(windows);

        println!("cargo:rerun-if-changed=common-controls.manifest");
        tauri_build::try_build(attrs).expect("failed to run tauri build script");

        // Match upstream's test-binary manifest handling: app_manifest above
        // applies to the app binary only, not the lib/integration test runners.
        // TaskDialogIndirect requires Common Controls v6 before any test runs.
        let manifest_path = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"),
        )
        .join("common-controls.manifest");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
            manifest_path.display()
        );
        // Tauri already embeds the app's resource, so avoid a duplicate there.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        println!("cargo:rerun-if-changed={}", manifest_path.display());
    }

    #[cfg(not(target_os = "windows"))]
    tauri_build::build();
}
