//! Bounded, shallow metadata inventory for one folder selected by a native dialog.
//! Navigation opens only explicitly requested observed folders; file contents are never opened.

use disk_steward_core::InventoryItemId;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_SAFE_JS_INTEGER: u64 = 9_007_199_254_740_991;

const MAX_RAW_ENTRIES: usize = 512;
const MAX_DURATION: Duration = Duration::from_secs(2);
const MAX_LABEL_CHARS: usize = 120;
const MAX_NAVIGATION_DEPTH: usize = 16;
const MAX_PAGES_PER_DIRECTORY: u32 = 128;

#[cfg(target_os = "macos")]
// macOS SDK sys/stat.h defines SF_DATALESS as 0x40000000 (Xcode 16 SDK).
const SF_DATALESS: u32 = 0x4000_0000;

#[cfg(unix)]
// Unix stat reports allocation in 512-byte units, not st_blksize units.
fn allocated_bytes_from_blocks<T>(blocks: T) -> Option<u64>
where
    u64: TryFrom<T>,
{
    u64::try_from(blocks).ok()?.checked_mul(512)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInventoryEntry {
    pub id: InventoryItemId,
    pub name: String,
    pub kind: &'static str,
    pub logical_bytes: Option<String>,
    pub allocated_bytes: Option<String>,
    pub link_count: Option<String>,
    pub availability: &'static str,
    pub hard_linked: bool,
    pub can_open: bool,
    pub navigation_blocker: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInventorySnapshot {
    pub observation_id: InventoryItemId,
    pub root_label: String,
    pub breadcrumbs: Vec<String>,
    pub entries: Vec<FolderInventoryEntry>,
    pub enumeration_complete: bool,
    pub unreadable_count: u32,
    pub stop_reason: Option<&'static str>,
    pub directories_not_traversed: u32,
    pub regular_file_logical_bytes: String,
    pub can_go_back: bool,
    pub page_number: u32,
    pub entries_seen: String,
    pub can_continue: bool,
}

fn empty_snapshot(
    observation_id: InventoryItemId,
    root_label: String,
    breadcrumbs: Vec<String>,
    can_go_back: bool,
) -> FolderInventorySnapshot {
    FolderInventorySnapshot {
        observation_id,
        root_label,
        breadcrumbs,
        entries: Vec::new(),
        enumeration_complete: false,
        unreadable_count: 0,
        stop_reason: None,
        directories_not_traversed: 0,
        regular_file_logical_bytes: "0".to_owned(),
        can_go_back,
        page_number: 1,
        entries_seen: "0".to_owned(),
        can_continue: false,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavigationRequest {
    expected_observation_id: String,
    entry_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContinueInventoryRequest {
    expected_observation_id: InventoryItemId,
}

#[derive(Clone, Default)]
pub struct FolderInventoryState(Arc<Mutex<Option<Session>>>);

impl FolderInventoryState {
    /// Copies only the latest explicitly observed, immediate file and folder
    /// metadata for a matching backend-held observation. It never reads files.
    #[cfg(unix)]
    pub(crate) fn blocked_items_for_observation(
        &self,
        expected_observation_id: InventoryItemId,
    ) -> Result<Vec<disk_steward_plan_model::InventoryItem>, &'static str> {
        let state = self.0.lock().map_err(|_| "observation_unavailable")?;
        let session = state.as_ref().ok_or("observation_unavailable")?;
        if session.observation_id != expected_observation_id
            || session.snapshot.observation_id != expected_observation_id
        {
            return Err("stale_observation");
        }

        use disk_steward_plan_model::{
            Blocker, Choice, EvidenceRef, InventoryItem, ItemKind, Suggestion,
        };
        let observation = serde_json::to_value(expected_observation_id)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or("observation_unavailable")?;
        let breadcrumb_text = session
            .snapshot
            .breadcrumbs
            .last()
            .map(String::as_str)
            .unwrap_or("selected folder");
        let mut items = Vec::new();
        let mut known_total = 0u64;
        for entry in &session.snapshot.entries {
            let kind = match entry.kind {
                "file" => ItemKind::File,
                "folder" => ItemKind::Folder,
                _ => continue,
            };
            let Some(observed) = session.entries.iter().find(|row| row.id == entry.id) else {
                continue;
            };
            if observed.identity.device != session.root_device
                || (kind == ItemKind::Folder
                    && observed.file_type != rustix::fs::FileType::Directory)
                || (kind == ItemKind::File
                    && observed.file_type != rustix::fs::FileType::RegularFile)
            {
                continue;
            }
            let logical_bytes = match entry.logical_bytes.as_deref() {
                Some(value) if kind == ItemKind::File => {
                    let parsed = value.parse::<u64>().map_err(|_| "unsafe_observed_size")?;
                    if parsed > MAX_SAFE_JS_INTEGER {
                        return Err("unsafe_observed_size");
                    }
                    known_total = known_total
                        .checked_add(parsed)
                        .filter(|total| *total <= MAX_SAFE_JS_INTEGER)
                        .ok_or("unsafe_observed_size")?;
                    Some(parsed)
                }
                Some(_) => return Err("unsafe_observed_size"),
                None => None,
            };
            let mut blockers = vec![
                Blocker {
                    code: "metadata_only".to_owned(),
                    reason: "Only immediate entry metadata was observed; no file contents or cleanup eligibility were established.".to_owned(),
                },
                Blocker {
                    code: "incomplete_coverage".to_owned(),
                    reason: "This observation is shallow and does not represent all contents of the selected storage scope.".to_owned(),
                },
            ];
            if kind == ItemKind::Folder {
                blockers.push(Blocker {
                    code: "folder_children_unknown".to_owned(),
                    reason: "The folder's children were not imported or inspected recursively."
                        .to_owned(),
                });
            }
            if entry.availability == "dataless" {
                blockers.push(Blocker {
                    code: "dataless_placeholder".to_owned(),
                    reason: "The entry is marked dataless. Provider hydration and content availability are unknown; no contents were opened.".to_owned(),
                });
            }
            items.push(InventoryItem {
                id: entry.id,
                parent_id: None,
                label: entry.name.clone(),
                kind,
                logical_bytes,
                recovery_lower_bytes: None,
                recovery_upper_bytes: None,
                storage_identity: None,
                blockers,
                evidence: vec![EvidenceRef {
                    id: disk_steward_core::EvidenceId::new(),
                    summary: format!(
                        "Observation {observation} at {breadcrumb_text}. Shallow metadata only; no paths or contents retained. Availability: {}; logical bytes: {}; allocated bytes: {}; link count: {}. Provider and hydration behavior are unverified. Allocation is per-entry st_blocks metadata; hard links or shared storage may overlap, so this is not a unique or reclaimable total.",
                        entry.availability,
                        entry.logical_bytes.as_deref().unwrap_or("unknown"),
                        entry.allocated_bytes.as_deref().unwrap_or("unknown"),
                        entry.link_count.as_deref().unwrap_or("unknown"),
                    ),
                }],
                suggestion: Suggestion {
                    choice: Choice::Keep,
                    rationale: "Keep by default until a person reviews evidence and the storage scope.".to_owned(),
                },
            });
        }
        if items.is_empty() {
            return Err("no_importable_entries");
        }
        Ok(items)
    }

    #[cfg(not(unix))]
    pub(crate) fn blocked_items_for_observation(
        &self,
        _: InventoryItemId,
    ) -> Result<Vec<disk_steward_plan_model::InventoryItem>, &'static str> {
        Err("observation_unavailable")
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
struct DirectoryFrame {
    fd: std::os::fd::OwnedFd,
    label: String,
    identity: DirectoryIdentity,
    opened_from: Option<(Vec<u8>, DirectoryIdentity)>,
}

#[cfg(unix)]
struct ObservedEntry {
    id: InventoryItemId,
    name: Vec<u8>,
    identity: DirectoryIdentity,
    file_type: rustix::fs::FileType,
    can_open: bool,
}

#[cfg(unix)]
struct Session {
    root_device: u64,
    frames: Vec<DirectoryFrame>,
    observation_id: InventoryItemId,
    entries: Vec<ObservedEntry>,
    snapshot: FolderInventorySnapshot,
    cursor: Option<DirectoryCursor>,
}

#[cfg(not(unix))]
struct Session;

#[cfg(unix)]
struct ScannedDirectory {
    snapshot: FolderInventorySnapshot,
    entries: Vec<ObservedEntry>,
    cursor: Option<DirectoryCursor>,
}

#[cfg(unix)]
struct DirectoryCursor {
    directory: rustix::fs::Dir,
    baseline: rustix::fs::Stat,
    identity: DirectoryIdentity,
    page_number: u32,
    entries_seen: u64,
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
        && is_dataless(before) == is_dataless(after)
}

#[cfg(unix)]
fn identity(stat: &rustix::fs::Stat) -> DirectoryIdentity {
    DirectoryIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
    }
}

#[cfg(target_os = "macos")]
fn is_dataless(stat: &rustix::fs::Stat) -> bool {
    stat.st_flags & SF_DATALESS != 0
}

#[cfg(all(unix, not(target_os = "macos")))]
fn is_dataless(_: &rustix::fs::Stat) -> bool {
    false
}

#[cfg(unix)]
fn availability(stat: &rustix::fs::Stat) -> &'static str {
    if is_dataless(stat) {
        "dataless"
    } else {
        "unknown"
    }
}

#[cfg(unix)]
fn refuse_dataless_directory(stat: &rustix::fs::Stat) -> Result<(), &'static str> {
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory
        && is_dataless(stat)
    {
        Err("This folder is marked dataless. Choose another folder to inspect.")
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn folder_navigation_blocker(
    stat: &rustix::fs::Stat,
    name: &[u8],
    root_device: u64,
    depth: usize,
) -> Option<&'static str> {
    if is_dataless(stat) {
        Some("dataless")
    } else if stat.st_dev as u64 != root_device {
        Some("cross_device")
    } else if name.first() == Some(&b'.') {
        Some("hidden")
    } else if is_managed_boundary(name) {
        Some("managed_boundary")
    } else if depth >= MAX_NAVIGATION_DEPTH {
        Some("depth_limit")
    } else {
        None
    }
}

#[cfg(unix)]
fn directory_navigation_policy(
    stat: &rustix::fs::Stat,
    name: &[u8],
    root_device: u64,
    depth: usize,
) -> (bool, Option<&'static str>) {
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory {
        return (false, None);
    }
    let blocker = folder_navigation_blocker(stat, name, root_device, depth);
    (blocker.is_none(), blocker)
}

#[cfg(unix)]
fn is_managed_boundary(name: &[u8]) -> bool {
    [
        b".app".as_slice(),
        b".bundle",
        b".framework",
        b".photoslibrary",
        b".photolibrary",
        b".musiclibrary",
        b".keychain-db",
    ]
    .iter()
    .any(|suffix| {
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    })
}

#[cfg(unix)]
fn path_has_managed_boundary(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.components().any(|component| {
        matches!(component, std::path::Component::Normal(name) if is_managed_boundary(name.as_bytes()))
    })
}

#[cfg(all(test, unix))]
fn scan_selected_folder(path: &Path) -> Result<FolderInventorySnapshot, &'static str> {
    scan_selected_folder_with_limits(path, MAX_RAW_ENTRIES, MAX_DURATION)
}

#[cfg(all(test, unix))]
fn scan_selected_folder_with_limits(
    path: &Path,
    max_entries: usize,
    duration: Duration,
) -> Result<FolderInventorySnapshot, &'static str> {
    use rustix::fs::{fstat, open, statat, AtFlags, FileType, Mode, OFlags, CWD};
    use std::os::fd::OwnedFd;

    let root_label = path_label(path);
    if path_has_managed_boundary(path) {
        return Err("This managed application or library folder is not traversed.");
    }
    let selected_stat = statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| "The selected folder metadata is unavailable.")?;
    refuse_dataless_directory(&selected_stat)?;
    let root_fd: OwnedFd = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "The selected folder could not be opened safely.")?;
    let root_before =
        fstat(&root_fd).map_err(|_| "The selected folder metadata is unavailable.")?;
    let root_id = identity(&root_before);
    if FileType::from_raw_mode(root_before.st_mode) != FileType::Directory {
        return Err("The selected location is not a directory.");
    }
    refuse_dataless_directory(&root_before)?;
    let frame = DirectoryFrame {
        fd: root_fd,
        label: root_label.clone(),
        identity: root_id,
        opened_from: None,
    };
    let scanned = scan_directory_fd(ScanRequest {
        fd: &frame.fd,
        expected_identity: frame.identity,
        root_device: root_id.device,
        root_label: &root_label,
        breadcrumbs: vec![root_label.clone()],
        can_go_back: false,
        depth: 1,
        max_entries,
        duration,
        cursor: None,
        page_number: 1,
        entries_seen: 0,
    })?;
    Ok(scanned.snapshot)
}

#[cfg(unix)]
struct ScanRequest<'a> {
    fd: &'a std::os::fd::OwnedFd,
    expected_identity: DirectoryIdentity,
    root_device: u64,
    root_label: &'a str,
    breadcrumbs: Vec<String>,
    can_go_back: bool,
    depth: usize,
    max_entries: usize,
    duration: Duration,
    cursor: Option<DirectoryCursor>,
    page_number: u32,
    entries_seen: u64,
}

#[cfg(unix)]
fn scan_directory_fd(request: ScanRequest<'_>) -> Result<ScannedDirectory, &'static str> {
    let ScanRequest {
        fd,
        expected_identity,
        root_device,
        root_label,
        breadcrumbs,
        can_go_back,
        depth,
        max_entries,
        duration,
        cursor,
        page_number,
        entries_seen,
    } = request;
    use rustix::fs::{fstat, statat, AtFlags, Dir, FileType};

    let started = Instant::now();
    let mut snapshot = empty_snapshot(
        InventoryItemId::new(),
        root_label.to_owned(),
        breadcrumbs,
        can_go_back,
    );
    snapshot.page_number = page_number;
    snapshot.entries_seen = entries_seen.to_string();
    let root_metadata_result = fstat(fd);
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: Vec::new(),
            cursor: None,
        });
    }
    let root_before =
        root_metadata_result.map_err(|_| "The selected folder metadata is unavailable.")?;
    if identity(&root_before) != expected_identity
        || identity(&root_before).device != root_device
        || FileType::from_raw_mode(root_before.st_mode) != FileType::Directory
    {
        return Err("The selected folder changed. Choose it again to inspect it.");
    }
    refuse_dataless_directory(&root_before)?;
    let mut pending_cursor = cursor;
    let cursor_baseline = pending_cursor
        .as_ref()
        .map(|cursor| cursor.baseline)
        .unwrap_or(root_before);
    if let Some(cursor) = pending_cursor.as_ref() {
        if cursor.identity != expected_identity
            || !root_metadata_matches(&cursor_baseline, &root_before)
        {
            return Err("Folder changed while paging. Choose the folder again.");
        }
    }
    let directory_result = match pending_cursor.take() {
        Some(cursor) => Ok(cursor.directory),
        None => Dir::read_from(fd),
    };
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: Vec::new(),
            cursor: None,
        });
    }
    let mut directory =
        directory_result.map_err(|_| "The selected folder could not be read safely.")?;
    let mut raw_entries = 0usize;
    let mut entries_seen = entries_seen;
    let mut logical_bytes = 0u64;
    let mut observed_entries = Vec::new();

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
        entries_seen = entries_seen.saturating_add(1);
        if started.elapsed() >= duration {
            snapshot.stop_reason = Some("time_limit");
            break;
        }
        let metadata = statat(fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW);
        if started.elapsed() >= duration {
            snapshot.stop_reason = Some("time_limit");
            break;
        }
        let label = safe_label_bytes(raw_name);
        let id = InventoryItemId::new();
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(_) => {
                snapshot.unreadable_count = snapshot.unreadable_count.saturating_add(1);
                snapshot.entries.push(FolderInventoryEntry {
                    id,
                    name: label,
                    kind: "unreadable",
                    logical_bytes: None,
                    allocated_bytes: None,
                    link_count: None,
                    availability: "unknown",
                    hard_linked: false,
                    can_open: false,
                    navigation_blocker: None,
                });
                snapshot
                    .stop_reason
                    .get_or_insert("entry_metadata_unavailable");
                continue;
            }
        };
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        let entry_identity = identity(&metadata);
        let (kind, entry_logical_bytes) = if entry_identity.device != root_device {
            if file_type == FileType::Directory {
                snapshot.directories_not_traversed =
                    snapshot.directories_not_traversed.saturating_add(1);
                ("folder", None)
            } else {
                ("unsupported", None)
            }
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
        let (allocated_bytes, link_count) = if kind == "file" {
            (
                allocated_bytes_from_blocks(metadata.st_blocks).map(|bytes| bytes.to_string()),
                Some(metadata.st_nlink.to_string()),
            )
        } else {
            (None, None)
        };
        let (can_open, navigation_blocker) =
            directory_navigation_policy(&metadata, raw_name, root_device, depth);
        snapshot.entries.push(FolderInventoryEntry {
            id,
            name: label,
            kind,
            logical_bytes: entry_logical_bytes.map(|size| size.to_string()),
            allocated_bytes,
            link_count,
            availability: availability(&metadata),
            hard_linked: kind == "file" && metadata.st_nlink > 1,
            can_open,
            navigation_blocker,
        });
        observed_entries.push(ObservedEntry {
            id,
            name: raw_name.to_vec(),
            identity: entry_identity,
            file_type,
            can_open,
        });
        if raw_entries == max_entries {
            if snapshot.stop_reason.is_none() {
                snapshot.stop_reason = Some(if page_number >= MAX_PAGES_PER_DIRECTORY {
                    "page_limit"
                } else {
                    "entry_limit"
                });
            }
            break;
        }
    }

    snapshot.regular_file_logical_bytes = logical_bytes.to_string();
    snapshot.entries_seen = entries_seen.to_string();
    if started.elapsed() >= duration {
        snapshot.enumeration_complete = false;
        snapshot.stop_reason.get_or_insert("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: observed_entries,
            cursor: None,
        });
    }
    let root_after = fstat(fd);
    if started.elapsed() >= duration {
        snapshot.enumeration_complete = false;
        snapshot.stop_reason.get_or_insert("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: observed_entries,
            cursor: None,
        });
    }
    let stable = match &root_after {
        Ok(after) => root_metadata_matches(&cursor_baseline, after),
        Err(_) => false,
    };
    if let Ok(after) = &root_after {
        refuse_dataless_directory(after)?;
    }
    match root_after {
        Ok(_) if stable => {}
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
    let continue_paging = snapshot.stop_reason == Some("entry_limit")
        && stable
        && page_number < MAX_PAGES_PER_DIRECTORY;
    snapshot.can_continue = continue_paging;
    let cursor = continue_paging.then_some(DirectoryCursor {
        directory,
        baseline: cursor_baseline,
        identity: expected_identity,
        page_number,
        entries_seen,
    });
    Ok(ScannedDirectory {
        snapshot,
        entries: observed_entries,
        cursor,
    })
}

#[cfg(not(unix))]
fn scan_selected_folder(_: &Path) -> Result<FolderInventorySnapshot, &'static str> {
    Err("Selected-folder inventory is unsupported on this platform.")
}

#[cfg(unix)]
fn selected_root_session(
    path: &Path,
    max_entries: usize,
    duration: Duration,
) -> Result<(Session, FolderInventorySnapshot), &'static str> {
    use rustix::fs::{fstat, open, FileType, Mode, OFlags};
    use std::os::fd::OwnedFd;

    let started = Instant::now();
    use rustix::fs::{statat, AtFlags, CWD};
    if path_has_managed_boundary(path) {
        return Err("This managed application or library folder is not traversed.");
    }
    let selected_stat = statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW);
    if started.elapsed() >= duration {
        return Err("The selected folder metadata could not be read within the inventory limit.");
    }
    let selected_stat =
        selected_stat.map_err(|_| "The selected folder metadata is unavailable.")?;
    refuse_dataless_directory(&selected_stat)?;
    let root_label = path_label(path);
    let opened = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    );
    if started.elapsed() >= duration {
        return Err("The selected folder could not be opened within the inventory limit.");
    }
    let fd: OwnedFd = opened.map_err(|_| "The selected folder could not be opened safely.")?;
    let root_stat = fstat(&fd);
    if started.elapsed() >= duration {
        return Err("The selected folder metadata could not be read within the inventory limit.");
    }
    let root_stat = root_stat.map_err(|_| "The selected folder metadata is unavailable.")?;
    if FileType::from_raw_mode(root_stat.st_mode) != FileType::Directory {
        return Err("The selected location is not a directory.");
    }
    refuse_dataless_directory(&root_stat)?;
    let root_identity = identity(&root_stat);
    let frame = DirectoryFrame {
        fd,
        label: root_label.clone(),
        identity: root_identity,
        opened_from: None,
    };
    let remaining = duration.saturating_sub(started.elapsed());
    let scanned = scan_directory_fd(ScanRequest {
        fd: &frame.fd,
        expected_identity: frame.identity,
        root_device: root_identity.device,
        root_label: &root_label,
        breadcrumbs: vec![root_label.clone()],
        can_go_back: false,
        depth: 1,
        max_entries,
        duration: remaining,
        cursor: None,
        page_number: 1,
        entries_seen: 0,
    })?;
    let session = Session {
        root_device: root_identity.device,
        frames: vec![frame],
        observation_id: scanned.snapshot.observation_id,
        entries: scanned.entries,
        snapshot: scanned.snapshot.clone(),
        cursor: scanned.cursor,
    };
    Ok((session, scanned.snapshot))
}

#[cfg(unix)]
fn validate_session_bindings(
    session: &Session,
    started: Instant,
    duration: Duration,
) -> Result<(), &'static str> {
    use rustix::fs::{fstat, statat, AtFlags, FileType};
    use std::ffi::CString;

    for (index, frame) in session.frames.iter().enumerate() {
        if started.elapsed() >= duration {
            return Err("Folder observation expired. Inspect the folder again.");
        }
        let stat = fstat(&frame.fd);
        if started.elapsed() >= duration {
            return Err("Folder observation expired. Inspect the folder again.");
        }
        let stat = stat.map_err(|_| "Folder observation changed. Inspect the folder again.")?;
        refuse_dataless_directory(&stat)?;
        if identity(&stat) != frame.identity
            || frame.identity.device != session.root_device
            || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        {
            return Err("Folder observation changed. Inspect the folder again.");
        }
        if index == 0 {
            if frame.opened_from.is_some() {
                return Err("Folder observation changed. Inspect the folder again.");
            }
            continue;
        }
        let (name, parent_identity) = frame
            .opened_from
            .as_ref()
            .ok_or("Folder observation changed. Inspect the folder again.")?;
        let parent = &session.frames[index - 1];
        if parent.identity != *parent_identity {
            return Err("Folder observation changed. Inspect the folder again.");
        }
        if started.elapsed() >= duration {
            return Err("Folder observation expired. Inspect the folder again.");
        }
        let name = CString::new(name.as_slice())
            .map_err(|_| "Folder observation changed. Inspect the folder again.")?;
        let stat = statat(&parent.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW);
        if started.elapsed() >= duration {
            return Err("Folder observation expired. Inspect the folder again.");
        }
        let stat = stat.map_err(|_| "Folder observation changed. Inspect the folder again.")?;
        refuse_dataless_directory(&stat)?;
        if identity(&stat) != frame.identity
            || stat.st_dev as u64 != session.root_device
            || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        {
            return Err("Folder observation changed. Inspect the folder again.");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_child_binding(
    parent: &DirectoryFrame,
    child: &DirectoryFrame,
    root_device: u64,
    started: Instant,
    duration: Duration,
) -> Result<(), &'static str> {
    use rustix::fs::{fstat, statat, AtFlags, FileType};
    use std::ffi::CString;

    let (name, parent_identity) = child
        .opened_from
        .as_ref()
        .ok_or("Folder observation changed. Inspect the folder again.")?;
    if parent.identity != *parent_identity || started.elapsed() >= duration {
        return Err("Folder observation changed. Inspect the folder again.");
    }
    let parent_stat = fstat(&parent.fd);
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let parent_stat =
        parent_stat.map_err(|_| "Folder observation changed. Inspect the folder again.")?;
    if identity(&parent_stat) != parent.identity {
        return Err("Folder observation changed. Inspect the folder again.");
    }
    let name = CString::new(name.as_slice())
        .map_err(|_| "Folder observation changed. Inspect the folder again.")?;
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let named_stat = statat(&parent.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW);
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let named_stat =
        named_stat.map_err(|_| "Folder observation changed. Inspect the folder again.")?;
    refuse_dataless_directory(&named_stat)?;
    if identity(&named_stat) != child.identity
        || named_stat.st_dev as u64 != root_device
        || FileType::from_raw_mode(named_stat.st_mode) != FileType::Directory
    {
        return Err("Folder observation changed. Inspect the folder again.");
    }
    let held_stat = fstat(&child.fd);
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let held_stat =
        held_stat.map_err(|_| "Folder observation changed. Inspect the folder again.")?;
    refuse_dataless_directory(&held_stat)?;
    if identity(&held_stat) != child.identity
        || held_stat.st_dev as u64 != root_device
        || FileType::from_raw_mode(held_stat.st_mode) != FileType::Directory
    {
        return Err("Folder observation changed. Inspect the folder again.");
    }
    Ok(())
}

#[cfg(unix)]
fn navigate_session(
    session: &mut Session,
    request: NavigationRequest,
    max_entries: usize,
    duration: Duration,
) -> Result<FolderInventorySnapshot, &'static str> {
    use rustix::fs::{fstat, openat, statat, AtFlags, FileType, Mode, OFlags};
    use std::ffi::CString;

    let started = Instant::now();
    let expected_observation: InventoryItemId = request
        .expected_observation_id
        .parse()
        .map_err(|_| "Folder observation is stale. Inspect the folder again.")?;
    if expected_observation != session.observation_id {
        return Err("Folder observation is stale. Inspect the folder again.");
    }
    validate_session_bindings(session, started, duration)?;
    let current_index = session.frames.len().saturating_sub(1);
    if request.entry_id.is_none() {
        if current_index == 0 {
            return Err("There is no parent folder to return to.");
        }
        let parent = &session.frames[current_index - 1];
        let crumbs = session.frames[..current_index]
            .iter()
            .map(|frame| frame.label.clone())
            .collect::<Vec<_>>();
        let scanned = scan_directory_fd(ScanRequest {
            fd: &parent.fd,
            expected_identity: parent.identity,
            root_device: session.root_device,
            root_label: &parent.label,
            breadcrumbs: crumbs,
            can_go_back: current_index > 1,
            depth: current_index,
            max_entries,
            duration: duration.saturating_sub(started.elapsed()),
            cursor: None,
            page_number: 1,
            entries_seen: 0,
        })?;
        validate_session_bindings(session, started, duration)?;
        session.frames.truncate(current_index);
        session.observation_id = scanned.snapshot.observation_id;
        session.entries = scanned.entries;
        session.snapshot = scanned.snapshot.clone();
        session.cursor = scanned.cursor;
        return Ok(scanned.snapshot);
    }

    if session.frames.len() >= MAX_NAVIGATION_DEPTH {
        return Err("The folder navigation depth limit was reached.");
    }
    let requested_entry: InventoryItemId = request
        .entry_id
        .as_deref()
        .unwrap_or_default()
        .parse()
        .map_err(|_| "Folder entry is stale. Inspect the folder again.")?;
    let observed = session
        .entries
        .iter()
        .find(|entry| entry.id == requested_entry)
        .ok_or("Folder entry is stale. Inspect the folder again.")?;
    if !observed.can_open || observed.file_type != FileType::Directory {
        return Err("This folder cannot be opened from the current observation.");
    }
    let parent = session.frames.last().expect("session has a root frame");
    let name = CString::new(observed.name.as_slice())
        .map_err(|_| "Folder entry is stale. Inspect the folder again.")?;
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let named_stat = statat(&parent.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW);
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let named_stat = named_stat.map_err(|_| "Folder entry changed. Inspect the folder again.")?;
    refuse_dataless_directory(&named_stat)?;
    if identity(&named_stat) != observed.identity
        || named_stat.st_dev as u64 != session.root_device
        || FileType::from_raw_mode(named_stat.st_mode) != FileType::Directory
    {
        return Err("Folder entry changed. Inspect the folder again.");
    }
    let opened = openat(
        &parent.fd,
        name.as_c_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    );
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let child_fd = opened.map_err(|_| "Folder entry changed. Inspect the folder again.")?;
    let child_stat = fstat(&child_fd);
    if started.elapsed() >= duration {
        return Err("Folder observation expired. Inspect the folder again.");
    }
    let child_stat = child_stat.map_err(|_| "Folder entry changed. Inspect the folder again.")?;
    refuse_dataless_directory(&child_stat)?;
    if identity(&child_stat) != observed.identity
        || child_stat.st_dev as u64 != session.root_device
        || FileType::from_raw_mode(child_stat.st_mode) != FileType::Directory
    {
        return Err("Folder entry changed. Inspect the folder again.");
    }
    let label = safe_label_bytes(&observed.name);
    let child = DirectoryFrame {
        fd: child_fd,
        label,
        identity: observed.identity,
        opened_from: Some((observed.name.clone(), parent.identity)),
    };
    let mut crumbs = session
        .frames
        .iter()
        .map(|frame| frame.label.clone())
        .collect::<Vec<_>>();
    crumbs.push(child.label.clone());
    let scanned = scan_directory_fd(ScanRequest {
        fd: &child.fd,
        expected_identity: child.identity,
        root_device: session.root_device,
        root_label: &child.label,
        breadcrumbs: crumbs,
        can_go_back: true,
        depth: session.frames.len() + 1,
        max_entries,
        duration: duration.saturating_sub(started.elapsed()),
        cursor: None,
        page_number: 1,
        entries_seen: 0,
    })?;
    validate_session_bindings(session, started, duration)?;
    validate_child_binding(parent, &child, session.root_device, started, duration)?;
    session.frames.push(child);
    session.observation_id = scanned.snapshot.observation_id;
    session.entries = scanned.entries;
    session.snapshot = scanned.snapshot.clone();
    session.cursor = scanned.cursor;
    Ok(scanned.snapshot)
}

#[cfg(unix)]
fn continue_session(
    session: &mut Session,
    request: ContinueInventoryRequest,
    max_entries: usize,
    duration: Duration,
) -> Result<FolderInventorySnapshot, &'static str> {
    let started = Instant::now();
    if request.expected_observation_id != session.observation_id {
        return Err("Folder observation is stale. Inspect the folder again.");
    }
    if let Err(error) = validate_session_bindings(session, started, duration) {
        session.cursor.take();
        session.snapshot.can_continue = false;
        return Err(error);
    }
    let cursor = session
        .cursor
        .take()
        .ok_or("This folder page cannot continue. Choose the folder again to inspect it.")?;
    if !session.snapshot.can_continue
        || session.snapshot.page_number != cursor.page_number
        || session.snapshot.entries_seen != cursor.entries_seen.to_string()
    {
        session.snapshot.can_continue = false;
        return Err("Folder paging state changed. Choose the folder again to inspect it.");
    }
    let Some(next_page) = cursor.page_number.checked_add(1) else {
        session.snapshot.can_continue = false;
        return Err("Folder paging limit reached. Choose the folder again to inspect it.");
    };
    let entries_seen = cursor.entries_seen;
    let frame = session.frames.last().expect("session has a root frame");
    let scanned = scan_directory_fd(ScanRequest {
        fd: &frame.fd,
        expected_identity: cursor.identity,
        root_device: session.root_device,
        root_label: &frame.label,
        breadcrumbs: session
            .frames
            .iter()
            .map(|frame| frame.label.clone())
            .collect(),
        can_go_back: session.frames.len() > 1,
        depth: session.frames.len(),
        max_entries,
        duration: duration.saturating_sub(started.elapsed()),
        cursor: Some(cursor),
        page_number: next_page,
        entries_seen,
    });
    let scanned = match scanned {
        Ok(scanned) => scanned,
        Err(error) => {
            session.snapshot.can_continue = false;
            return Err(error);
        }
    };
    if let Err(error) = validate_session_bindings(session, started, duration) {
        session.snapshot.can_continue = false;
        return Err(error);
    }
    session.observation_id = scanned.snapshot.observation_id;
    session.entries = scanned.entries;
    session.snapshot = scanned.snapshot.clone();
    session.cursor = scanned.cursor;
    Ok(scanned.snapshot)
}

#[cfg(not(unix))]
fn navigate_session(
    _: &mut Session,
    _: NavigationRequest,
    _: usize,
    _: Duration,
) -> Result<FolderInventorySnapshot, &'static str> {
    Err("Folder navigation is unsupported on this platform.")
}

#[cfg(not(unix))]
fn continue_session(
    _: &mut Session,
    _: ContinueInventoryRequest,
    _: usize,
    _: Duration,
) -> Result<FolderInventorySnapshot, &'static str> {
    Err("Folder paging is unsupported on this platform.")
}

#[cfg(not(unix))]
fn selected_root_session(
    _: &Path,
    _: usize,
    _: Duration,
) -> Result<(Session, FolderInventorySnapshot), &'static str> {
    Err("Selected-folder inventory is unsupported on this platform.")
}

/// Opens one native folder picker and returns a bounded shallow metadata view.
#[tauri::command]
pub async fn inspect_selected_folder_entries(
    app: tauri::AppHandle,
    state: tauri::State<'_, FolderInventoryState>,
) -> Result<Option<FolderInventorySnapshot>, String> {
    use tauri_plugin_dialog::DialogExt;

    let guard = crate::NativeCheckGuard::acquire()?;
    let sessions = Arc::clone(&state.0);
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
        let (session, snapshot) =
            selected_root_session(&path, MAX_RAW_ENTRIES, MAX_DURATION).map_err(str::to_owned)?;
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder inventory state is unavailable.".to_owned())?;
        *current = Some(session);
        Ok(Some(snapshot))
    })
    .await
    .map_err(|_| "Selected-folder inventory did not complete.".to_owned())?
}

#[tauri::command]
pub async fn navigate_folder_inventory(
    state: tauri::State<'_, FolderInventoryState>,
    request: NavigationRequest,
) -> Result<FolderInventorySnapshot, String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let sessions = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder inventory state is unavailable.".to_owned())?;
        let session = current
            .as_mut()
            .ok_or_else(|| "Choose a folder before navigating its inventory.".to_owned())?;
        navigate_session(session, request, MAX_RAW_ENTRIES, MAX_DURATION).map_err(str::to_owned)
    })
    .await
    .map_err(|_| "Folder navigation did not complete.".to_owned())?
}

#[tauri::command]
pub async fn continue_folder_inventory(
    state: tauri::State<'_, FolderInventoryState>,
    request: ContinueInventoryRequest,
) -> Result<FolderInventorySnapshot, String> {
    let guard = crate::NativeCheckGuard::acquire()?;
    let sessions = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut current = sessions
            .lock()
            .map_err(|_| "Folder inventory state is unavailable.".to_owned())?;
        let session = current
            .as_mut()
            .ok_or_else(|| "Choose a folder before continuing its inventory.".to_owned())?;
        continue_session(session, request, MAX_RAW_ENTRIES, MAX_DURATION).map_err(str::to_owned)
    })
    .await
    .map_err(|_| "Folder page continuation did not complete.".to_owned())?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs as unix_fs;
    use tempfile::tempdir;

    fn typed_id_string(id: InventoryItemId) -> String {
        serde_json::to_value(id)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn navigate_into(
        session: &mut Session,
        name: &str,
    ) -> Result<FolderInventorySnapshot, &'static str> {
        let entry = session
            .entries
            .iter()
            .find(|entry| safe_label_bytes(&entry.name) == name)
            .expect("fixture entry");
        let request = NavigationRequest {
            expected_observation_id: typed_id_string(session.observation_id),
            entry_id: Some(typed_id_string(entry.id)),
        };
        navigate_session(session, request, MAX_RAW_ENTRIES, MAX_DURATION)
    }

    fn go_back(session: &mut Session) -> Result<FolderInventorySnapshot, &'static str> {
        let request = NavigationRequest {
            expected_observation_id: typed_id_string(session.observation_id),
            entry_id: None,
        };
        navigate_session(session, request, MAX_RAW_ENTRIES, MAX_DURATION)
    }

    fn continue_current_page(
        session: &mut Session,
        max_entries: usize,
    ) -> Result<FolderInventorySnapshot, &'static str> {
        let expected_observation_id = session.observation_id;
        continue_session(
            session,
            ContinueInventoryRequest {
                expected_observation_id,
            },
            max_entries,
            MAX_DURATION,
        )
    }

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
        assert!(entries.iter().all(|entry| entry.availability == "unknown"));
        assert!(entries.iter().any(|entry| entry.kind == "folder"));
        assert!(entries.iter().any(|entry| entry.kind == "symlink"));
        assert!(entries
            .iter()
            .filter(|entry| entry.name.starts_with("one"))
            .all(|entry| entry.hard_linked));
        let regular_files = entries
            .iter()
            .filter(|entry| entry.name.starts_with("one"))
            .collect::<Vec<_>>();
        assert_eq!(regular_files.len(), 2);
        assert_eq!(
            regular_files[0].allocated_bytes,
            regular_files[1].allocated_bytes
        );
        assert_eq!(regular_files[0].link_count, regular_files[1].link_count);
        assert!(regular_files[0].allocated_bytes.is_some());
        assert_eq!(regular_files[0].link_count.as_deref(), Some("2"));
        let folder = entries.iter().find(|entry| entry.kind == "folder").unwrap();
        let link = entries
            .iter()
            .find(|entry| entry.kind == "symlink")
            .unwrap();
        assert_eq!(folder.allocated_bytes, None);
        assert_eq!(folder.link_count, None);
        assert_eq!(folder.navigation_blocker, None);
        assert_eq!(link.allocated_bytes, None);
        assert_eq!(link.link_count, None);
        assert_eq!(link.navigation_blocker, None);

        let file_json = serde_json::to_value(regular_files[0]).unwrap();
        assert_eq!(
            file_json["allocatedBytes"],
            regular_files[0].allocated_bytes.clone().unwrap()
        );
        assert_eq!(file_json["linkCount"], "2");
        let folder_json = serde_json::to_value(folder).unwrap();
        assert!(folder_json["allocatedBytes"].is_null());
        assert!(folder_json["linkCount"].is_null());
        let escaped = entries
            .iter()
            .find(|entry| entry.name.starts_with("odd"))
            .unwrap();
        assert!(escaped.name.contains("\\u{000A}"));
        assert!(escaped.name.contains("\\u{202E}"));
    }

    #[test]
    fn allocated_bytes_conversion_is_checked_and_unknown_on_invalid_blocks() {
        assert_eq!(allocated_bytes_from_blocks(0i64), Some(0));
        assert_eq!(allocated_bytes_from_blocks(1i64), Some(512));
        assert_eq!(allocated_bytes_from_blocks(17i64), Some(8_704));
        assert_eq!(allocated_bytes_from_blocks(-1i64), None);
        assert_eq!(allocated_bytes_from_blocks(i64::MAX), None);
        let maximum_blocks = u64::MAX / 512;
        assert_eq!(
            allocated_bytes_from_blocks(maximum_blocks),
            Some(maximum_blocks * 512)
        );
        assert_eq!(allocated_bytes_from_blocks(maximum_blocks + 1), None);
    }

    #[test]
    fn folder_navigation_explanations_match_openability_policy() {
        use rustix::fs::{statat, AtFlags, CWD};

        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("normal")).unwrap();
        let stat = statat(CWD, root.path().join("normal"), AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let root_device = stat.st_dev as u64;
        assert_eq!(
            folder_navigation_blocker(&stat, b"normal", root_device, 1),
            None
        );
        assert_eq!(
            folder_navigation_blocker(&stat, b".hidden", root_device, 1),
            Some("hidden")
        );
        assert_eq!(
            folder_navigation_blocker(&stat, b"Mock.app", root_device, 1),
            Some("managed_boundary")
        );
        assert_eq!(
            folder_navigation_blocker(&stat, b"normal", root_device, MAX_NAVIGATION_DEPTH),
            Some("depth_limit")
        );
        assert_eq!(
            folder_navigation_blocker(&stat, b"normal", root_device ^ 1, 1),
            Some("cross_device")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_dataless_flag_is_exact_and_blocks_directory_enumeration() {
        use rustix::fs::{statat, AtFlags, CWD};

        let root = tempdir().unwrap();
        let stat = statat(CWD, root.path(), AtFlags::SYMLINK_NOFOLLOW).unwrap();
        assert_eq!(availability(&stat), "unknown");
        let mut marked = stat;
        marked.st_flags = SF_DATALESS | 0x1;
        assert!(is_dataless(&marked));
        assert_eq!(availability(&marked), "dataless");
        assert!(refuse_dataless_directory(&marked).is_err());
        marked.st_flags = 0x1;
        assert!(!is_dataless(&marked));
        assert_eq!(availability(&marked), "unknown");
        assert!(refuse_dataless_directory(&marked).is_ok());
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
    fn continuation_returns_fresh_bounded_pages_and_an_explicit_empty_end_page() {
        let root = tempdir().unwrap();
        for index in 0..6 {
            fs::write(root.path().join(format!("item-{index}")), b"x").unwrap();
        }
        let (mut session, first) = selected_root_session(root.path(), 3, MAX_DURATION).unwrap();
        assert_eq!(first.page_number, 1);
        assert_eq!(first.entries.len(), 3);
        assert_eq!(first.entries_seen, "3");
        assert!(first.can_continue);

        let stale = ContinueInventoryRequest {
            expected_observation_id: InventoryItemId::new(),
        };
        assert!(continue_session(&mut session, stale, 3, MAX_DURATION).is_err());
        assert!(session.cursor.is_some());
        assert_eq!(session.snapshot.observation_id, first.observation_id);

        let second = continue_current_page(&mut session, 3).unwrap();
        assert_eq!(second.page_number, 2);
        assert_eq!(second.entries.len(), 3);
        assert_eq!(second.entries_seen, "6");
        assert!(second.can_continue);
        assert_ne!(second.observation_id, first.observation_id);

        let end = continue_current_page(&mut session, 3).unwrap();
        assert_eq!(end.page_number, 3);
        assert!(end.entries.is_empty());
        assert_eq!(end.entries_seen, "6");
        assert!(end.enumeration_complete);
        assert!(!end.can_continue);
        assert_ne!(end.observation_id, second.observation_id);

        let mut names = first
            .entries
            .iter()
            .chain(second.entries.iter())
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 6);
        assert!(names.iter().all(|name| name.starts_with("item-")));
        assert!(end.entries.len() <= 3);
    }

    #[test]
    fn changed_directory_invalidates_cursor_and_navigation_replaces_it() {
        let root = tempdir().unwrap();
        for index in 0..4 {
            fs::write(root.path().join(format!("root-{index}")), b"x").unwrap();
        }
        fs::create_dir(root.path().join("child")).unwrap();
        for index in 0..3 {
            fs::write(
                root.path().join("child").join(format!("child-{index}")),
                b"x",
            )
            .unwrap();
        }
        let (mut session, _) = selected_root_session(root.path(), 512, MAX_DURATION).unwrap();
        let child_id = session
            .entries
            .iter()
            .find(|entry| entry.name == b"child")
            .unwrap()
            .id;
        let request = NavigationRequest {
            expected_observation_id: typed_id_string(session.observation_id),
            entry_id: Some(typed_id_string(child_id)),
        };
        let child_page = navigate_session(&mut session, request, 1, MAX_DURATION).unwrap();
        assert!(child_page.can_continue);
        assert!(session.cursor.is_some());

        let back = NavigationRequest {
            expected_observation_id: typed_id_string(session.observation_id),
            entry_id: None,
        };
        let parent = navigate_session(&mut session, back, 512, MAX_DURATION).unwrap();
        assert!(!parent.can_continue);
        assert!(session.cursor.is_none());

        let (mut changing, first) = selected_root_session(root.path(), 2, MAX_DURATION).unwrap();
        assert!(first.can_continue);
        fs::write(root.path().join("later-entry"), b"x").unwrap();
        assert!(continue_current_page(&mut changing, 2).is_err());
        assert!(changing.cursor.is_none());
        assert!(!changing.snapshot.can_continue);

        let moved_root = tempdir().unwrap();
        fs::create_dir(moved_root.path().join("child")).unwrap();
        for index in 0..3 {
            fs::write(
                moved_root
                    .path()
                    .join("child")
                    .join(format!("item-{index}")),
                b"x",
            )
            .unwrap();
        }
        let (mut moved, _) = selected_root_session(moved_root.path(), 512, MAX_DURATION).unwrap();
        let child_id = moved
            .entries
            .iter()
            .find(|entry| entry.name == b"child")
            .unwrap()
            .id;
        let enter = NavigationRequest {
            expected_observation_id: typed_id_string(moved.observation_id),
            entry_id: Some(typed_id_string(child_id)),
        };
        let child_page = navigate_session(&mut moved, enter, 1, MAX_DURATION).unwrap();
        assert!(child_page.can_continue);
        fs::rename(
            moved_root.path().join("child"),
            moved_root.path().join("renamed-child"),
        )
        .unwrap();
        assert!(continue_current_page(&mut moved, 1).is_err());
        assert!(moved.cursor.is_none());
        assert!(!moved.snapshot.can_continue);
    }

    #[test]
    fn page_limit_closes_cursor_after_128_responses() {
        let root = tempdir().unwrap();
        for index in 0..MAX_PAGES_PER_DIRECTORY {
            fs::write(root.path().join(format!("item-{index:03}")), b"x").unwrap();
        }
        let (mut session, mut page) = selected_root_session(root.path(), 1, MAX_DURATION).unwrap();
        for expected_page in 2..=MAX_PAGES_PER_DIRECTORY {
            page = continue_current_page(&mut session, 1).unwrap();
            assert_eq!(page.page_number, expected_page);
            assert_eq!(page.entries.len(), 1);
        }
        assert_eq!(page.entries_seen, MAX_PAGES_PER_DIRECTORY.to_string());
        assert_eq!(page.stop_reason, Some("page_limit"));
        assert!(!page.can_continue);
        assert!(session.cursor.is_none());
    }

    #[test]
    fn continuation_request_rejects_unknown_fields() {
        let id = typed_id_string(InventoryItemId::new());
        let valid = serde_json::json!({ "expectedObservationId": id });
        assert!(serde_json::from_value::<ContinueInventoryRequest>(valid).is_ok());
        let with_path = serde_json::json!({
            "expectedObservationId": typed_id_string(InventoryItemId::new()),
            "path": "/tmp"
        });
        assert!(serde_json::from_value::<ContinueInventoryRequest>(with_path).is_err());
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

    #[test]
    fn descriptor_navigation_and_back_create_fresh_pathless_observations() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("child")).unwrap();
        fs::write(root.path().join("child/note.txt"), b"unchanged").unwrap();
        let (mut session, first) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        assert_eq!(first.breadcrumbs, vec![path_label(root.path())]);
        let child = session.entries.iter().find(|entry| entry.can_open).unwrap();
        let child_name = safe_label_bytes(&child.name);
        let child_snapshot = navigate_into(&mut session, &child_name).unwrap();
        assert!(child_snapshot.can_go_back);
        assert_eq!(child_snapshot.breadcrumbs.last(), Some(&child_name));
        assert_ne!(child_snapshot.observation_id, first.observation_id);
        assert!(child_snapshot
            .entries
            .iter()
            .any(|entry| entry.name == "note.txt"));
        let parent_snapshot = go_back(&mut session).unwrap();
        assert!(!parent_snapshot.can_go_back);
        assert_eq!(parent_snapshot.breadcrumbs, first.breadcrumbs);
        assert_ne!(
            parent_snapshot.observation_id,
            child_snapshot.observation_id
        );
        let json = serde_json::to_string(&parent_snapshot).unwrap();
        assert!(!json.contains(root.path().to_str().unwrap()));
        assert_eq!(
            fs::read(root.path().join("child/note.txt")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn stale_observation_does_not_change_the_retained_session() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("child")).unwrap();
        let (mut session, first) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        navigate_into(&mut session, "child").unwrap();
        let current = session.observation_id;
        let request = NavigationRequest {
            expected_observation_id: typed_id_string(first.observation_id),
            entry_id: None,
        };
        assert!(navigate_session(&mut session, request, MAX_RAW_ENTRIES, MAX_DURATION).is_err());
        assert_eq!(session.observation_id, current);
        assert_eq!(session.frames.len(), 2);
    }

    #[test]
    fn replacing_observed_directory_with_directory_or_symlink_refuses_navigation() {
        for replace_with_symlink in [false, true] {
            let root = tempdir().unwrap();
            let child = root.path().join("child");
            fs::create_dir(&child).unwrap();
            let (mut session, _) =
                selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
            let old_observation = session.observation_id;
            let old_entry = session.entries[0].id;
            let moved = root.path().join("moved");
            fs::rename(&child, &moved).unwrap();
            if replace_with_symlink {
                unix_fs::symlink(&moved, &child).unwrap();
            } else {
                fs::create_dir(&child).unwrap();
            }
            let request = NavigationRequest {
                expected_observation_id: typed_id_string(old_observation),
                entry_id: Some(typed_id_string(old_entry)),
            };
            assert!(
                navigate_session(&mut session, request, MAX_RAW_ENTRIES, MAX_DURATION).is_err()
            );
            assert_eq!(session.observation_id, old_observation);
            assert_eq!(session.frames.len(), 1);
        }
    }

    #[test]
    fn moved_child_refuses_back_and_retains_previous_snapshot() {
        let root = tempdir().unwrap();
        let external = tempdir().unwrap();
        fs::create_dir(root.path().join("child")).unwrap();
        let (mut session, _) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        navigate_into(&mut session, "child").unwrap();
        let observation = session.observation_id;
        fs::rename(
            root.path().join("child"),
            external.path().join("moved-child"),
        )
        .unwrap();
        assert!(go_back(&mut session).is_err());
        assert_eq!(session.observation_id, observation);
        assert_eq!(session.frames.len(), 2);
    }

    #[test]
    fn hidden_and_managed_directories_are_not_openable_and_device_mismatch_refuses() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(".hidden")).unwrap();
        fs::create_dir(root.path().join("App.app")).unwrap();
        fs::create_dir(root.path().join("normal")).unwrap();
        let (mut session, _) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        assert!(
            !session
                .entries
                .iter()
                .find(|entry| safe_label_bytes(&entry.name) == ".hidden")
                .unwrap()
                .can_open
        );
        assert_eq!(
            session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.name == ".hidden")
                .unwrap()
                .navigation_blocker,
            Some("hidden")
        );
        assert!(
            !session
                .entries
                .iter()
                .find(|entry| safe_label_bytes(&entry.name) == "App.app")
                .unwrap()
                .can_open
        );
        assert_eq!(
            session
                .snapshot
                .entries
                .iter()
                .find(|entry| entry.name == "App.app")
                .unwrap()
                .navigation_blocker,
            Some("managed_boundary")
        );
        let simulated_device = session.root_device.saturating_add(1);
        let normal = session
            .entries
            .iter_mut()
            .find(|entry| safe_label_bytes(&entry.name) == "normal")
            .unwrap();
        normal.identity.device = simulated_device;
        let normal_id = normal.id;
        let observation_id = session.observation_id;
        let request = NavigationRequest {
            expected_observation_id: typed_id_string(observation_id),
            entry_id: Some(typed_id_string(normal_id)),
        };
        assert!(navigate_session(&mut session, request, MAX_RAW_ENTRIES, MAX_DURATION).is_err());
        assert_eq!(session.observation_id, observation_id);
        let managed_root = root.path().join("Photo.photoslibrary");
        fs::create_dir(&managed_root).unwrap();
        assert!(selected_root_session(&managed_root, MAX_RAW_ENTRIES, MAX_DURATION).is_err());
        let app_contents = root.path().join("Mock.app/Contents");
        fs::create_dir_all(&app_contents).unwrap();
        assert!(selected_root_session(&app_contents, MAX_RAW_ENTRIES, MAX_DURATION).is_err());
    }

    #[test]
    fn navigation_stops_at_sixteen_directory_levels() {
        let root = tempdir().unwrap();
        let mut current = root.path().to_path_buf();
        for index in 0..16 {
            current.push(format!("level-{index}"));
            fs::create_dir(&current).unwrap();
        }
        let (mut session, _) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        for _ in 0..MAX_NAVIGATION_DEPTH - 1 {
            let entry = session.entries.iter().find(|entry| entry.can_open).unwrap();
            let request = NavigationRequest {
                expected_observation_id: typed_id_string(session.observation_id),
                entry_id: Some(typed_id_string(entry.id)),
            };
            navigate_session(&mut session, request, MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        }
        assert_eq!(session.frames.len(), MAX_NAVIGATION_DEPTH);
        assert!(session.entries.iter().all(|entry| !entry.can_open));
        assert!(session
            .snapshot
            .entries
            .iter()
            .filter(|entry| entry.kind == "folder")
            .all(|entry| entry.navigation_blocker == Some("depth_limit")));
    }

    #[test]
    fn navigation_request_rejects_renderer_path_fields() {
        for extra in [
            serde_json::json!({"path": "/tmp/forbidden"}),
            serde_json::json!({"approved": true}),
        ] {
            let mut value = serde_json::json!({
                "expectedObservationId": typed_id_string(InventoryItemId::new()),
                "entryId": null
            });
            for (key, value_to_add) in extra.as_object().unwrap() {
                value[key] = value_to_add.clone();
            }
            assert!(serde_json::from_value::<NavigationRequest>(value).is_err());
        }
        let invalid = serde_json::json!({
            "expectedObservationId": "not-a-typed-id",
            "entryId": null
        });
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("child")).unwrap();
        let (mut session, snapshot) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        assert!(navigate_session(
            &mut session,
            serde_json::from_value(invalid).unwrap(),
            MAX_RAW_ENTRIES,
            MAX_DURATION,
        )
        .is_err());
        assert_eq!(session.observation_id, snapshot.observation_id);
    }

    #[test]
    fn import_mapping_keeps_only_file_and_folder_metadata_blocked_by_default() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("observed.txt"), b"abc").unwrap();
        fs::create_dir(root.path().join("child-folder")).unwrap();
        unix_fs::symlink("observed.txt", root.path().join("alias-link")).unwrap();
        let (session, snapshot) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        let state = FolderInventoryState::default();
        *state.0.lock().unwrap() = Some(session);

        let imported = state
            .blocked_items_for_observation(snapshot.observation_id)
            .unwrap();
        assert_eq!(imported.len(), 2);
        assert!(imported.iter().all(|item| item.parent_id.is_none()));
        assert!(imported
            .iter()
            .all(|item| item.recovery_lower_bytes.is_none()));
        assert!(imported
            .iter()
            .all(|item| item.recovery_upper_bytes.is_none()));
        assert!(imported.iter().all(|item| item.storage_identity.is_none()));
        assert!(imported
            .iter()
            .all(|item| item.suggestion.choice == disk_steward_plan_model::Choice::Keep));
        assert!(imported.iter().all(|item| {
            item.blockers
                .iter()
                .any(|blocker| blocker.code == "metadata_only")
                && item
                    .blockers
                    .iter()
                    .any(|blocker| blocker.code == "incomplete_coverage")
        }));
        let folder = imported
            .iter()
            .find(|item| item.kind == disk_steward_plan_model::ItemKind::Folder)
            .unwrap();
        assert!(folder
            .blockers
            .iter()
            .any(|blocker| blocker.code == "folder_children_unknown"));
        let evidence = &imported[0].evidence[0].summary;
        assert!(imported
            .iter()
            .all(|item| item.evidence[0].summary.len() <= 2048));
        assert!(imported.iter().any(|item| {
            item.evidence[0]
                .summary
                .contains("allocated bytes: unknown")
                && item.evidence[0].summary.contains("link count: unknown")
        }));
        assert!(imported.iter().any(|item| {
            item.evidence[0].summary.contains("allocated bytes: ")
                && !item.evidence[0]
                    .summary
                    .contains("allocated bytes: unknown")
                && item.evidence[0].summary.contains("link count: 1")
        }));
        let observation_id = serde_json::to_value(snapshot.observation_id)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        assert!(evidence.contains(&observation_id));
        assert!(!evidence.contains(root.path().to_str().unwrap()));

        let stale = state.blocked_items_for_observation(InventoryItemId::new());
        assert_eq!(stale.unwrap_err(), "stale_observation");
    }

    #[test]
    fn import_mapping_refuses_empty_supported_set_and_unsafe_integer_sizes() {
        let root = tempdir().unwrap();
        unix_fs::symlink("missing", root.path().join("link-only")).unwrap();
        let (session, snapshot) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        let state = FolderInventoryState::default();
        *state.0.lock().unwrap() = Some(session);
        assert_eq!(
            state
                .blocked_items_for_observation(snapshot.observation_id)
                .unwrap_err(),
            "no_importable_entries"
        );

        let sized_root = tempdir().unwrap();
        fs::write(sized_root.path().join("large"), b"x").unwrap();
        let (mut session, snapshot) =
            selected_root_session(sized_root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        let file = session
            .snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.kind == "file")
            .unwrap();
        file.logical_bytes = Some((MAX_SAFE_JS_INTEGER + 1).to_string());
        let state = FolderInventoryState::default();
        *state.0.lock().unwrap() = Some(session);
        assert_eq!(
            state
                .blocked_items_for_observation(snapshot.observation_id)
                .unwrap_err(),
            "unsafe_observed_size"
        );
    }

    #[test]
    fn dataless_file_import_stays_blocked_and_records_unknown_provider_state() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("placeholder"), b"unchanged").unwrap();
        let (mut session, snapshot) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        let file = session
            .snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.kind == "file")
            .unwrap();
        file.availability = "dataless";
        let state = FolderInventoryState::default();
        *state.0.lock().unwrap() = Some(session);
        let items = state
            .blocked_items_for_observation(snapshot.observation_id)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].suggestion.choice,
            disk_steward_plan_model::Choice::Keep
        );
        assert!(items[0]
            .blockers
            .iter()
            .any(|blocker| blocker.code == "dataless_placeholder"));
        assert!(items[0].storage_identity.is_none());
        assert!(items[0].evidence[0]
            .summary
            .contains("Availability: dataless"));
        assert!(items[0].evidence[0]
            .summary
            .contains("Provider and hydration behavior are unverified"));
    }

    #[test]
    fn native_selection_inside_a_known_managed_boundary_is_refused() {
        let root = tempdir().unwrap();
        for name in ["Sample.app", "Photos.PHOTOSLIBRARY"] {
            let inside = root.path().join(name).join("Contents");
            fs::create_dir_all(&inside).unwrap();
            fs::write(inside.join("valuable"), b"unchanged").unwrap();
            assert!(selected_root_session(&inside, MAX_RAW_ENTRIES, MAX_DURATION).is_err());
            assert_eq!(fs::read(inside.join("valuable")).unwrap(), b"unchanged");
        }
    }

    #[test]
    fn moved_ancestor_blocks_further_drill_down_without_changing_session() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(root.path().join("child/grandchild/deeper")).unwrap();
        let (mut session, _) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        navigate_into(&mut session, "child").unwrap();
        navigate_into(&mut session, "grandchild").unwrap();
        let previous = session.observation_id;
        fs::rename(root.path().join("child"), outside.path().join("moved")).unwrap();
        assert!(navigate_into(&mut session, "deeper").is_err());
        assert_eq!(session.observation_id, previous);
        assert_eq!(session.frames.len(), 3);
        assert!(go_back(&mut session).is_err());
    }

    #[test]
    fn expired_navigation_preserves_session_and_allows_a_fresh_explicit_retry() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("child")).unwrap();
        let (mut session, snapshot) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        let entry = session.entries.iter().find(|entry| entry.can_open).unwrap();
        let request = NavigationRequest {
            expected_observation_id: typed_id_string(snapshot.observation_id),
            entry_id: Some(typed_id_string(entry.id)),
        };
        assert!(navigate_session(&mut session, request, MAX_RAW_ENTRIES, Duration::ZERO).is_err());
        assert_eq!(session.observation_id, snapshot.observation_id);
        assert_eq!(session.frames.len(), 1);
        assert!(navigate_into(&mut session, "child").is_ok());
    }

    #[test]
    fn observed_metadata_to_durable_review_stays_blocked_and_replay_is_atomic() {
        use disk_steward_plan_model::{Choice, PlanEdit};
        use disk_steward_plan_store::{FixtureScale, GeneralPlanStore};
        let root = tempdir().unwrap();
        fs::write(root.path().join("valuable.txt"), b"unchanged").unwrap();
        let (session, snapshot) =
            selected_root_session(root.path(), MAX_RAW_ENTRIES, MAX_DURATION).unwrap();
        let state = FolderInventoryState::default();
        *state.0.lock().unwrap() = Some(session);
        let data = tempdir().unwrap();
        let database = data.path().join("live.sqlite3");
        let mut store = GeneralPlanStore::open(&database, FixtureScale::EmptyReview).unwrap();
        let empty = store.get_plan().unwrap();
        let imported = store
            .append_items(
                empty.revision_id,
                state
                    .blocked_items_for_observation(snapshot.observation_id)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(imported.items.len(), 1);
        assert!(!imported.execution_available);
        assert_eq!(imported.totals.selected_count, 0);
        assert!(store
            .append_items(
                imported.revision_id,
                state
                    .blocked_items_for_observation(snapshot.observation_id)
                    .unwrap()
            )
            .is_err());
        assert_eq!(store.get_plan().unwrap(), imported);
        let changed = store
            .edit(
                imported.revision_id,
                PlanEdit::SetChoice {
                    item_id: imported.items[0].item_id,
                    choice: Choice::Include,
                },
            )
            .unwrap()
            .snapshot;
        assert_eq!(changed.totals.selected_count, 0);
        assert!(!changed.items[0].effective_selected);
        let review = store.freeze_review(changed.revision_id).unwrap();
        drop(store);
        let reopened = GeneralPlanStore::open(&database, FixtureScale::EmptyReview).unwrap();
        assert_eq!(reopened.get_saved_review(review.review_id).unwrap(), review);
        assert_eq!(
            fs::read(root.path().join("valuable.txt")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn continuation_imports_only_the_new_page_and_expiry_invalidates_the_cursor() {
        let root = tempdir().unwrap();
        for index in 0..5 {
            fs::write(root.path().join(format!("item-{index}")), b"x").unwrap();
        }
        let (mut session, first) = selected_root_session(root.path(), 2, MAX_DURATION).unwrap();
        let second = continue_current_page(&mut session, 2).unwrap();
        let second_ids = second
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let state = FolderInventoryState::default();
        *state.0.lock().unwrap() = Some(session);
        assert_eq!(
            state
                .blocked_items_for_observation(first.observation_id)
                .unwrap_err(),
            "stale_observation"
        );
        let imported = state
            .blocked_items_for_observation(second.observation_id)
            .unwrap();
        assert_eq!(imported.len(), 2);
        assert!(imported.iter().all(|item| second_ids.contains(&item.id)));
        let mut guard = state.0.lock().unwrap();
        let session = guard.as_mut().unwrap();
        assert!(session.cursor.is_some());
        assert!(continue_session(
            session,
            ContinueInventoryRequest {
                expected_observation_id: second.observation_id
            },
            2,
            Duration::ZERO
        )
        .is_err());
        assert!(session.cursor.is_none());
        assert!(!session.snapshot.can_continue);
        assert_eq!(session.observation_id, second.observation_id);
        assert_eq!(session.snapshot.entries, second.entries);
    }

    #[test]
    fn cross_volume_directories_never_gain_navigation_from_unsupported_display_kind() {
        use rustix::fs::{statat, AtFlags, CWD};
        let root = tempdir().unwrap();
        let stat = statat(CWD, root.path(), AtFlags::SYMLINK_NOFOLLOW).unwrap();
        assert_eq!(
            directory_navigation_policy(&stat, b"normal", identity(&stat).device, 1),
            (true, None)
        );
        assert_eq!(
            directory_navigation_policy(&stat, b"normal", identity(&stat).device ^ 1, 1),
            (false, Some("cross_device"))
        );
        assert_eq!(
            directory_navigation_policy(&stat, b".hidden", identity(&stat).device, 1),
            (false, Some("hidden"))
        );
    }

    fn symlink_for_test(target: &Path, link: &Path) {
        unix_fs::symlink(target, link).unwrap();
    }
}
