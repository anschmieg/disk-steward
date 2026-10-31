fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "foundation_status",
            "check_selected_folder_access",
            "preview_native_review",
        ]),
    ))
    .expect("failed to generate application permission manifest");
}
