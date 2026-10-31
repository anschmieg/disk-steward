fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["foundation_status"])),
    )
    .expect("failed to generate application permission manifest");
}
