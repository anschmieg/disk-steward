//! Bounded metadata extraction for one native file picker selection.
//!
//! This module never opens or reads the selected file. The selected path is
//! used only for a no-follow metadata query and is never returned or persisted.

use std::path::Path;

use disk_steward_core::{EvidenceId, InventoryItemId};
use disk_steward_plan_model::{Blocker, Choice, EvidenceRef, InventoryItem, ItemKind, Suggestion};

const MAX_LABEL_CHARS: usize = 120;

pub(crate) fn inspect_selected_file(path: &Path) -> Result<InventoryItem, &'static str> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "The selected file metadata is unavailable. Nothing was added.")?;
    if !metadata.file_type().is_file() {
        return Err("Select a regular file; folders and symbolic links are unsupported.");
    }

    let label =
        bounded_basename(path).ok_or("The selected file has no usable name. Nothing was added.")?;

    Ok(InventoryItem {
        id: InventoryItemId::new(),
        parent_id: None,
        label,
        kind: ItemKind::File,
        logical_bytes: Some(metadata.len()),
        recovery_lower_bytes: None,
        recovery_upper_bytes: None,
        storage_identity: None,
        blockers: vec![Blocker {
            code: "native_metadata_only".to_owned(),
            reason: "Only the selected file's name and logical size were observed. This item is blocked from cleanup and does not establish file identity or operation authority.".to_owned(),
        }],
        evidence: vec![EvidenceRef {
            id: EvidenceId::new(),
            summary: "Native no-follow metadata observation; file contents were not opened.".to_owned(),
        }],
        suggestion: Suggestion {
            choice: Choice::Keep,
            rationale: "Keep by default until a supported inspection can establish trustworthy evidence.".to_owned(),
        },
    })
}

fn bounded_basename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }

    let sanitized: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        // Render directional formatting explicitly so a basename cannot visually
        // reorder an extension. This bounded label is never a source identity.
        .flat_map(|ch| {
            if matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                format!("\\u{{{:x}}}", ch as u32).chars().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .take(MAX_LABEL_CHARS)
        .collect();
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_only_bounded_basename_and_conservative_metadata() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("nested").join("installer.pkg");
        std::fs::create_dir(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"fixture bytes").unwrap();
        let original = std::fs::read(&source).unwrap();

        let item = inspect_selected_file(&source).unwrap();
        assert_eq!(item.label, "installer.pkg");
        assert!(!item.label.contains(fixture.path().to_str().unwrap()));
        assert_eq!(item.logical_bytes, Some(original.len() as u64));
        assert_eq!(item.kind, ItemKind::File);
        assert_eq!(item.parent_id, None);
        assert_eq!(item.recovery_lower_bytes, None);
        assert_eq!(item.recovery_upper_bytes, None);
        assert_eq!(item.storage_identity, None);
        assert_eq!(item.suggestion.choice, Choice::Keep);
        assert_eq!(item.blockers[0].code, "native_metadata_only");
        assert_eq!(std::fs::read(source).unwrap(), original);
    }

    #[test]
    fn rejects_directories_and_missing_paths() {
        let fixture = tempfile::tempdir().unwrap();
        assert!(inspect_selected_file(fixture.path()).is_err());
        assert!(inspect_selected_file(&fixture.path().join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_without_touching_the_sentinel() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        let sentinel = fixture.path().join("sentinel");
        let link = fixture.path().join("selected-link");
        std::fs::write(&sentinel, b"must remain unchanged").unwrap();
        symlink(&sentinel, &link).unwrap();

        assert!(inspect_selected_file(&link).is_err());
        assert_eq!(std::fs::read(sentinel).unwrap(), b"must remain unchanged");
    }

    #[test]
    fn directional_filename_controls_are_visible_and_cannot_reorder_extension() {
        let label = bounded_basename(Path::new("invoice\u{202e}gpj.exe")).unwrap();
        assert_eq!(label, "invoice\\u{202e}gpj.exe");
        assert!(!label.contains('\u{202e}'));
        let many = "\u{2066}".repeat(200);
        assert!(bounded_basename(Path::new(&many)).unwrap().chars().count() <= MAX_LABEL_CHARS);
    }

    #[test]
    fn sanitizes_control_chars_and_bounds_labels() {
        let name = format!("{}\nsecret", "x".repeat(MAX_LABEL_CHARS + 40));
        let path = Path::new("/temporary").join(name);
        let label = bounded_basename(&path).unwrap();
        assert_eq!(label, "x".repeat(MAX_LABEL_CHARS));
        assert!(!label.chars().any(char::is_control));
    }
}
