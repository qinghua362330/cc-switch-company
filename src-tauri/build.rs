fn main() {
    #[cfg(target_os = "windows")]
    {
        let windows = tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("common-controls.manifest"));
        let attrs = tauri_build::Attributes::new().windows_attributes(windows);

        println!("cargo:rerun-if-changed=common-controls.manifest");
        tauri_build::try_build(attrs).expect("failed to run tauri build script");
    }

    #[cfg(not(target_os = "windows"))]
    tauri_build::build();
}
