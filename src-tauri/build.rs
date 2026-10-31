fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "foundation_status",
            "check_selected_folder_access",
            "preview_native_review",
            "get_example_plan",
            "edit_example_plan",
            "freeze_example_review",
        ]),
    ))
    .expect("failed to generate application permission manifest");
}
