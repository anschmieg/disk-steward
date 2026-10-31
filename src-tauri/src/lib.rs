use disk_steward_core::{CapabilityReport, CapabilityStatus, HostPlatform, PlatformCapability};

/// Backend-owned state, deliberately not exposed as a generic frontend command.
pub fn foundation_capabilities() -> Vec<CapabilityReport> {
    let platform = match std::env::consts::OS {
        "macos" => HostPlatform::MacOS,
        "windows" => HostPlatform::Windows,
        "linux" => HostPlatform::Linux,
        _ => HostPlatform::Other,
    };
    [
        PlatformCapability::ScopeAccess,
        PlatformCapability::FileIdentity,
        PlatformCapability::RecoverableStage,
        PlatformCapability::Restore,
        PlatformCapability::PermanentRemoval,
        PlatformCapability::SecureCredentialStore,
        PlatformCapability::NativeConfirmation,
    ]
    .into_iter()
    .map(|capability| {
        CapabilityReport::new(
            platform,
            capability,
            CapabilityStatus::Unsupported {
                reason: "Production adapter not implemented or validated in M0".into(),
            },
        )
    })
    .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(foundation_capabilities())
        .invoke_handler(tauri::generate_handler![foundation_status])
        .run(tauri::generate_context!())
        .expect("error while running Disk Steward");
}

/// Read-only diagnostic bridge. Takes no paths, scopes, commands or credentials.
#[tauri::command]
fn foundation_status(reports: tauri::State<'_, Vec<CapabilityReport>>) -> Vec<(String, bool)> {
    reports
        .iter()
        .map(|report| (format!("{:?}", report.capability), report.is_available()))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn unimplemented_adapters_never_report_available() {
        let reports = super::foundation_capabilities();
        assert_eq!(reports.len(), 7);
        assert!(reports.iter().all(|report| !report.is_available()));
    }
}
