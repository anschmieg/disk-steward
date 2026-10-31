fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "foundation_status",
            "check_selected_folder_access",
            "preview_native_review",
            "get_example_plan",
            "edit_example_plan",
            "freeze_example_review",
            "preview_saved_example_review",
            "get_fixture_demo_status",
            "start_fixture_demo",
            "prepare_fixture_stage",
            "prepare_fixture_restore",
            "request_fixture_execution",
        ]),
    ))
    .expect("failed to generate application permission manifest");
}
