//! Bounded, shallow metadata inventory for one folder selected by a native dialog.
//! This command never descends into child folders or opens entry contents.

use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_RAW_ENTRIES: usize = 512;
const MAX_DURATION: Duration = Duration::from_secs(2);
const MAX_LABEL_CHARS: usize = 120;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInventoryEntry {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub logical_bytes: Option<String>,
    pub hard_linked: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInventorySnapshot {
    pub root_label: String,
    pub entries: Vec<FolderInventoryEntry>,
    pub enumeration_complete: bool,
    pub unreadable_count: u32,
    pub stop_reason: Option<&'static str>,
    pub directories_not_traversed: u32,
    pub regular_file_logical_bytes: String,
}

fn empty_snapshot(root_label: String) -> FolderInventorySnapshot {
    FolderInventorySnapshot {
        root_label,
        entries: Vec::new(),
        enumeration_complete: false,
        unreadable_count: 0,
        stop_reason: None,
        directories_not_traversed: 0,
        regular_file_logical_bytes: "0".to_owned(),
    }
}

#[cfg(unix)]
fn safe_label_bytes(raw: &[u8]) -> String {
    use std::str;

    let mut output = String::new();
    let mut remaining = raw;
    let mut truncated = false;
    while !remaining.is_empty() {
        match str::from_utf8(remaining) {
            Ok(valid) => {
                append_safe_text(&mut output, valid, &mut truncated);
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    let valid = str::from_utf8(&remaining[..valid_len])
                        .expect("UTF-8 decoder reports a valid prefix");
                    append_safe_text(&mut output, valid, &mut truncated);
                }
                let invalid_len = error.error_len().unwrap_or(remaining.len() - valid_len);
                for byte in &remaining[valid_len..valid_len + invalid_len] {
                    append_bounded(&mut output, &format!("\\x{byte:02X}"), &mut truncated);
                }
                remaining = &remaining[valid_len + invalid_len..];
            }
        }
        if truncated {
            break;
        }
    }
    if truncated {
        if output.chars().count() >= MAX_LABEL_CHARS {
            output.pop();
        }
        output.push('…');
    }
    output
}

#[cfg(unix)]
fn append_safe_text(output: &mut String, text: &str, truncated: &mut bool) {
    for character in text.chars() {
        let rendered = if character == '\\' {
            "\\\\".to_owned()
        } else if character.is_control() || is_bidi_control(character) {
            format!("\\u{{{:04X}}}", character as u32)
        } else {
            character.to_string()
        };
        append_bounded(output, &rendered, truncated);
        if *truncated {
            return;
        }
    }
}

#[cfg(unix)]
fn append_bounded(output: &mut String, text: &str, truncated: &mut bool) {
    if output.chars().count() + text.chars().count() > MAX_LABEL_CHARS {
        *truncated = true;
    } else {
        output.push_str(text);
    }
}

#[cfg(unix)]
fn is_bidi_control(character: char) -> bool {
    matches!(
        character as u32,
        0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x206F
    )
}

#[cfg(unix)]
fn path_label(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    path.file_name()
        .map(|name| safe_label_bytes(name.as_bytes()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Selected folder".to_owned())
}

#[cfg(unix)]
fn root_metadata_matches(before: &rustix::fs::Stat, after: &rustix::fs::Stat) -> bool {
    before.st_dev == after.st_dev
        && before.st_ino == after.st_ino
        && before.st_mode == after.st_mode
        && before.st_size == after.st_size
        && before.st_nlink == after.st_nlink
        && before.st_mtime == after.st_mtime
        && before.st_mtime_nsec == after.st_mtime_nsec
        && before.st_ctime == after.st_ctime
        && before.st_ctime_nsec == after.st_ctime_nsec
}

#[cfg(unix)]
fn scan_selected_folder(path: &Path) -> Result<FolderInventorySnapshot, &'static str> {
    scan_selected_folder_with_limits(path, MAX_RAW_ENTRIES, MAX_DURATION)
}

#[cfg(unix)]
fn scan_selected_folder_with_limits(
    path: &Path,
    max_entries: usize,
    duration: Duration,
) -> Result<FolderInventorySnapshot, &'static str> {
    use rustix::fs::{fstat, open, statat, AtFlags, Dir, FileType, Mode, OFlags};
    use std::os::fd::OwnedFd;

    let started = Instant::now();
    let root_label = path_label(path);
    let mut snapshot = empty_snapshot(root_label);
    let root_result = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    );
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(snapshot);
    }
    let root_fd: OwnedFd =
        root_result.map_err(|_| "The selected folder could not be opened safely.")?;
    let root_metadata_result = fstat(&root_fd);
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(snapshot);
    }
    let root_before =
        root_metadata_result.map_err(|_| "The selected folder metadata is unavailable.")?;
    let directory_result = Dir::read_from(&root_fd);
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(snapshot);
    }
    let mut directory =
        directory_result.map_err(|_| "The selected folder could not be read safely.")?;
    let mut raw_entries = 0usize;
    let mut logical_bytes = 0u64;

    loop {
        if started.elapsed() >= duration {
            snapshot.stop_reason = Some("time_limit");
            break;
        }
        let next = directory.read();
        if started.elapsed() >= duration {
            snapshot.stop_reason = Some("time_limit");
            break;
        }
        let Some(next) = next else {
            snapshot.enumeration_complete = true;
            break;
        };
        let entry = match next {
            Ok(entry) => entry,
            Err(_) => {
                snapshot.unreadable_count = snapshot.unreadable_count.saturating_add(1);
                snapshot.stop_reason = Some("directory_read_error");
                break;
            }
        };
        let raw_name = entry.file_name().to_bytes();
        if raw_name == b"." || raw_name == b".." {
            continue;
        }
        if raw_entries >= max_entries {
            snapshot.stop_reason = Some("entry_limit");
            break;
        }
        raw_entries += 1;
        if started.elapsed() >= duration {
            snapshot.stop_reason = Some("time_limit");
            break;
        }
        let metadata = statat(&root_fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW);
        if started.elapsed() >= duration {
            snapshot.stop_reason = Some("time_limit");
            break;
        }
        let label = safe_label_bytes(raw_name);
        let id = format!("entry-{:06}", raw_entries);
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(_) => {
                snapshot.unreadable_count = snapshot.unreadable_count.saturating_add(1);
                snapshot.entries.push(FolderInventoryEntry {
                    id,
                    name: label,
                    kind: "unreadable",
                    logical_bytes: None,
                    hard_linked: false,
                });
                snapshot
                    .stop_reason
                    .get_or_insert("entry_metadata_unavailable");
                continue;
            }
        };
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        let (kind, entry_logical_bytes) = if metadata.st_dev != root_before.st_dev {
            if file_type == FileType::Directory {
                snapshot.directories_not_traversed =
                    snapshot.directories_not_traversed.saturating_add(1);
            }
            ("unsupported", None)
        } else {
            match file_type {
                FileType::RegularFile => {
                    let size = u64::try_from(metadata.st_size).ok();
                    ("file", size)
                }
                FileType::Directory => {
                    snapshot.directories_not_traversed =
                        snapshot.directories_not_traversed.saturating_add(1);
                    ("folder", None)
                }
                FileType::Symlink => ("symlink", None),
                _ => ("other", None),
            }
        };
        if let Some(size) = entry_logical_bytes {
            match logical_bytes.checked_add(size) {
                Some(total) => logical_bytes = total,
                None => {
                    snapshot.stop_reason = Some("logical_size_overflow");
                    break;
                }
            }
        }
        snapshot.entries.push(FolderInventoryEntry {
            id,
            name: label,
            kind,
            logical_bytes: entry_logical_bytes.map(|size| size.to_string()),
            hard_linked: kind == "file" && metadata.st_nlink > 1,
        });
        if raw_entries == max_entries {
            snapshot.stop_reason = Some("entry_limit");
            break;
        }
    }

    snapshot.regular_file_logical_bytes = logical_bytes.to_string();
    if started.elapsed() >= duration {
        snapshot.enumeration_complete = false;
        snapshot.stop_reason.get_or_insert("time_limit");
        return Ok(snapshot);
    }
    let root_after = fstat(&root_fd);
    if started.elapsed() >= duration {
        snapshot.enumeration_complete = false;
        snapshot.stop_reason.get_or_insert("time_limit");
        return Ok(snapshot);
    }
    match root_after {
        Ok(after) if root_metadata_matches(&root_before, &after) => {}
        Ok(_) => {
            snapshot.enumeration_complete = false;
            snapshot.stop_reason = Some("root_metadata_changed");
        }
        Err(_) => {
            snapshot.enumeration_complete = false;
            snapshot.stop_reason = Some("root_metadata_unavailable");
        }
    }
    if snapshot.stop_reason.is_some() {
        snapshot.enumeration_complete = false;
    }
    Ok(snapshot)
}

#[cfg(not(unix))]
fn scan_selected_folder(_: &Path) -> Result<FolderInventorySnapshot, &'static str> {
    Err("Selected-folder inventory is unsupported on this platform.")
}

/// Opens one native folder picker and returns a bounded shallow metadata view.
#[tauri::command]
pub async fn inspect_selected_folder_entries(
    app: tauri::AppHandle,
) -> Result<Option<FolderInventorySnapshot>, String> {
    use tauri_plugin_dialog::DialogExt;

    let guard = crate::NativeCheckGuard::acquire()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let Some(selection) = app
            .dialog()
            .file()
            .set_title("Choose a folder for a read-only shallow inventory")
            .set_can_create_directories(false)
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let path = selection
            .into_path()
            .map_err(|_| "The selected location is not a supported local folder.".to_owned())?;
        scan_selected_folder(&path).map(Some).map_err(str::to_owned)
    })
    .await
    .map_err(|_| "Selected-folder inventory did not complete.".to_owned())?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs as unix_fs;
    use tempfile::tempdir;

    #[test]
    fn shallow_inventory_records_entry_kinds_sizes_links_and_escaped_names() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("one.txt"), b"abc").unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        symlink_for_test(&root.path().join("one.txt"), &root.path().join("link"));
        fs::hard_link(
            root.path().join("one.txt"),
            root.path().join("one-hardlink.txt"),
        )
        .unwrap();
        let odd_name = "odd\n\u{202e}";
        fs::write(root.path().join(odd_name), b"x").unwrap();

        let snapshot = scan_selected_folder(root.path()).unwrap();
        assert!(snapshot.enumeration_complete);
        assert_eq!(snapshot.stop_reason, None);
        assert_eq!(snapshot.entries.len(), 5);
        assert_eq!(snapshot.regular_file_logical_bytes, "7");
        assert_eq!(snapshot.directories_not_traversed, 1);
        let entries = &snapshot.entries;
        assert!(entries.iter().any(|entry| entry.kind == "folder"));
        assert!(entries.iter().any(|entry| entry.kind == "symlink"));
        assert!(entries
            .iter()
            .filter(|entry| entry.name.starts_with("one"))
            .all(|entry| entry.hard_linked));
        let escaped = entries
            .iter()
            .find(|entry| entry.name.starts_with("odd"))
            .unwrap();
        assert!(escaped.name.contains("\\u{000A}"));
        assert!(escaped.name.contains("\\u{202E}"));
    }

    #[test]
    fn refuses_missing_or_symlink_selected_roots() {
        let parent = tempdir().unwrap();
        let actual = parent.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let link = parent.path().join("link");
        unix_fs::symlink(&actual, &link).unwrap();
        assert!(scan_selected_folder(&parent.path().join("missing")).is_err());
        assert!(scan_selected_folder(&link).is_err());
    }

    #[test]
    fn entry_limit_returns_an_explicit_incomplete_snapshot() {
        let root = tempdir().unwrap();
        for index in 0..4 {
            fs::write(root.path().join(format!("entry-{index}")), b"x").unwrap();
        }
        let snapshot = scan_selected_folder_with_limits(root.path(), 2, MAX_DURATION).unwrap();
        assert_eq!(snapshot.entries.len(), 2);
        assert!(!snapshot.enumeration_complete);
        assert_eq!(snapshot.stop_reason, Some("entry_limit"));
    }

    #[test]
    fn labels_escape_invalid_bytes_backslashes_and_controls_with_a_bound() {
        assert_eq!(safe_label_bytes(&[0xff]), "\\xFF");
        assert_eq!(safe_label_bytes(b"\\xFF"), "\\\\xFF");
        assert_eq!(safe_label_bytes(b"line\n"), "line\\u{000A}");
        let long = safe_label_bytes(&vec![b'a'; 256]);
        assert!(long.chars().count() <= MAX_LABEL_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn zero_time_budget_is_incomplete_without_observed_entries() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("valuable"), b"unchanged").unwrap();
        let snapshot = scan_selected_folder_with_limits(root.path(), 512, Duration::ZERO).unwrap();
        assert!(!snapshot.enumeration_complete);
        assert_eq!(snapshot.stop_reason, Some("time_limit"));
        assert!(snapshot.entries.is_empty());
        assert_eq!(
            fs::read(root.path().join("valuable")).unwrap(),
            b"unchanged"
        );
    }

    fn symlink_for_test(target: &Path, link: &Path) {
        unix_fs::symlink(target, link).unwrap();
    }
}
