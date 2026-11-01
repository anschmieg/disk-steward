//! Bounded metadata extraction for one native file or folder selection.
//!
//! This module never opens or reads the selected entry or enumerates a selected
//! folder. The selected path is used only for a no-follow metadata query and is
//! never returned or persisted.

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

pub(crate) fn inspect_selected_folder(path: &Path) -> Result<InventoryItem, &'static str> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "The selected folder metadata is unavailable. Nothing was added.")?;
    if !metadata.file_type().is_dir() {
        return Err("Select a directory; files and symbolic links are unsupported.");
    }

    let label = bounded_basename(path)
        .ok_or("The selected folder has no usable name. Nothing was added.")?;

    Ok(InventoryItem {
        id: InventoryItemId::new(),
        parent_id: None,
        label,
        kind: ItemKind::Folder,
        logical_bytes: None,
        recovery_lower_bytes: None,
        recovery_upper_bytes: None,
        storage_identity: None,
        blockers: vec![
            Blocker {
                code: "native_metadata_only".to_owned(),
                reason: "Only the selected folder entry's name and directory metadata were observed. The folder is blocked from cleanup; its contents and identity were not established.".to_owned(),
            },
            Blocker {
                code: "contents_not_inventoried".to_owned(),
                reason: "The folder's children were not enumerated. Its contents and whether it is empty are unknown.".to_owned(),
            },
        ],
        evidence: vec![EvidenceRef {
            id: EvidenceId::new(),
            summary: "Native no-follow metadata observation of the selected directory entry. Children were not enumerated; contents and emptiness remain unknown.".to_owned(),
        }],
        suggestion: Suggestion {
            choice: Choice::Keep,
            rationale: "Keep by default until a supported inventory can establish trustworthy evidence about this folder and its contents.".to_owned(),
        },
    })
}

pub(crate) fn bounded_basename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }

    let mut visible: Vec<char> = name
        .chars()
        .flat_map(|ch| {
            if is_bidi_control(ch) {
                format!("\\u{{{:x}}}", ch as u32)
                    .chars()
                    .collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect();
    let first_nonspace = visible
        .iter()
        .position(|character| !character.is_whitespace());
    let last_nonspace = visible
        .iter()
        .rposition(|character| !character.is_whitespace());
    if let (Some(first), Some(last)) = (first_nonspace, last_nonspace) {
        let mut bounded = Vec::with_capacity(visible.len() + 16);
        for (index, character) in visible.drain(..).enumerate() {
            if (index < first || index > last) && character.is_whitespace() {
                bounded.extend(format!("\\u{{{:04x}}}", character as u32).chars());
            } else {
                bounded.push(character);
            }
        }
        visible = bounded;
    }
    let sanitized: String = visible.into_iter().take(MAX_LABEL_CHARS).collect();
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

/// All directional formatting characters escaped in native display labels and
/// confirmation locations. This is display hygiene, never identity matching.
pub(crate) fn is_bidi_control(ch: char) -> bool {
    matches!(ch as u32, 0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x206f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use disk_steward_plan_model::{synthetic_fixture, PlanEdit, PlanEditor};

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

    #[test]
    fn selected_folder_stays_unknown_blocked_and_does_not_touch_valuable_children() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("Important Folder");
        std::fs::create_dir(&folder).unwrap();
        let valuable_child = folder.join("valuable-notes.txt");
        std::fs::write(&valuable_child, b"keep these original bytes").unwrap();
        let original = std::fs::read(&valuable_child).unwrap();

        let item = inspect_selected_folder(&folder).unwrap();
        assert_eq!(item.kind, ItemKind::Folder);
        assert_eq!(item.label, "Important Folder");
        assert_eq!(item.parent_id, None);
        assert_eq!(item.logical_bytes, None);
        assert_eq!(item.recovery_lower_bytes, None);
        assert_eq!(item.recovery_upper_bytes, None);
        assert_eq!(item.storage_identity, None);
        assert_eq!(item.suggestion.choice, Choice::Keep);
        assert_eq!(item.blockers.len(), 2);
        assert!(item
            .blockers
            .iter()
            .any(|blocker| blocker.code == "native_metadata_only"));
        assert!(item
            .blockers
            .iter()
            .any(|blocker| blocker.code == "contents_not_inventoried"));
        assert!(item.evidence[0]
            .summary
            .contains("Children were not enumerated"));
        assert_eq!(std::fs::read(&valuable_child).unwrap(), original);

        let mut editor = PlanEditor::new(synthetic_fixture(9, 1).unwrap()).unwrap();
        let appended = editor
            .add_items(editor.snapshot().revision_id, vec![item.clone()])
            .unwrap();
        let attempt = editor
            .edit(
                appended.revision_id,
                PlanEdit::SetChoice {
                    item_id: item.id,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert!(!attempt.changed);
        assert_eq!(attempt.blocked.len(), 1);
        let projection = attempt
            .snapshot
            .items
            .iter()
            .find(|projected| projected.item_id == item.id)
            .unwrap();
        assert_eq!(projection.kind, ItemKind::Folder);
        assert_eq!(projection.logical_bytes, None);
        assert_eq!(projection.user_choice, None);
        assert!(!projection.effective_selected);
        assert_eq!(std::fs::read(&valuable_child).unwrap(), original);
    }

    #[test]
    fn rejects_folder_symlinks_files_and_missing_paths() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("directory");
        let file = fixture.path().join("file");
        let link = fixture.path().join("link");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(&file, b"fixture bytes").unwrap();
        #[cfg(unix)]
        symlink(&directory, &link).unwrap();

        assert!(inspect_selected_folder(&file).is_err());
        assert!(inspect_selected_folder(&fixture.path().join("missing")).is_err());
        #[cfg(unix)]
        assert!(inspect_selected_folder(&link).is_err());
    }
}
