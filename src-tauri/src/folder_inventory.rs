//! Bounded, shallow metadata inventory for one folder selected by a native dialog.
//! Navigation opens only explicitly requested observed folders; file contents are never opened.

use disk_steward_core::InventoryItemId;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_RAW_ENTRIES: usize = 512;
const MAX_DURATION: Duration = Duration::from_secs(2);
const MAX_LABEL_CHARS: usize = 120;
const MAX_NAVIGATION_DEPTH: usize = 16;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInventoryEntry {
    pub id: InventoryItemId,
    pub name: String,
    pub kind: &'static str,
    pub logical_bytes: Option<String>,
    pub hard_linked: bool,
    pub can_open: bool,
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
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavigationRequest {
    expected_observation_id: String,
    entry_id: Option<String>,
}

#[derive(Clone, Default)]
pub struct FolderInventoryState(Arc<Mutex<Option<Session>>>);

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
}

#[cfg(not(unix))]
struct Session;

#[cfg(unix)]
struct ScannedDirectory {
    snapshot: FolderInventorySnapshot,
    entries: Vec<ObservedEntry>,
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
fn identity(stat: &rustix::fs::Stat) -> DirectoryIdentity {
    DirectoryIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
    }
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
    use rustix::fs::{fstat, open, FileType, Mode, OFlags};
    use std::os::fd::OwnedFd;

    let root_label = path_label(path);
    if path_has_managed_boundary(path) {
        return Err("This managed application or library folder is not traversed.");
    }
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
    } = request;
    use rustix::fs::{fstat, statat, AtFlags, Dir, FileType};

    let started = Instant::now();
    let mut snapshot = empty_snapshot(
        InventoryItemId::new(),
        root_label.to_owned(),
        breadcrumbs,
        can_go_back,
    );
    let root_metadata_result = fstat(fd);
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: Vec::new(),
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
    let directory_result = Dir::read_from(fd);
    if started.elapsed() >= duration {
        snapshot.stop_reason = Some("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: Vec::new(),
        });
    }
    let mut directory =
        directory_result.map_err(|_| "The selected folder could not be read safely.")?;
    let mut raw_entries = 0usize;
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
                    hard_linked: false,
                    can_open: false,
                });
                snapshot
                    .stop_reason
                    .get_or_insert("entry_metadata_unavailable");
                continue;
            }
        };
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        let entry_identity = identity(&metadata);
        let is_directory = file_type == FileType::Directory;
        let (kind, entry_logical_bytes) = if entry_identity.device != root_device {
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
        let hidden = raw_name.first() == Some(&b'.');
        let can_open = is_directory
            && entry_identity.device == root_device
            && !hidden
            && !is_managed_boundary(raw_name)
            && depth < MAX_NAVIGATION_DEPTH;
        snapshot.entries.push(FolderInventoryEntry {
            id,
            name: label,
            kind,
            logical_bytes: entry_logical_bytes.map(|size| size.to_string()),
            hard_linked: kind == "file" && metadata.st_nlink > 1,
            can_open,
        });
        observed_entries.push(ObservedEntry {
            id,
            name: raw_name.to_vec(),
            identity: entry_identity,
            file_type,
            can_open,
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
        return Ok(ScannedDirectory {
            snapshot,
            entries: observed_entries,
        });
    }
    let root_after = fstat(fd);
    if started.elapsed() >= duration {
        snapshot.enumeration_complete = false;
        snapshot.stop_reason.get_or_insert("time_limit");
        return Ok(ScannedDirectory {
            snapshot,
            entries: observed_entries,
        });
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
    Ok(ScannedDirectory {
        snapshot,
        entries: observed_entries,
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
    if path_has_managed_boundary(path) {
        return Err("This managed application or library folder is not traversed.");
    }
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
    })?;
    let session = Session {
        root_device: root_identity.device,
        frames: vec![frame],
        observation_id: scanned.snapshot.observation_id,
        entries: scanned.entries,
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
        })?;
        validate_session_bindings(session, started, duration)?;
        session.frames.truncate(current_index);
        session.observation_id = scanned.snapshot.observation_id;
        session.entries = scanned.entries;
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
    })?;
    validate_session_bindings(session, started, duration)?;
    validate_child_binding(parent, &child, session.root_device, started, duration)?;
    session.frames.push(child);
    session.observation_id = scanned.snapshot.observation_id;
    session.entries = scanned.entries;
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
        assert!(
            !session
                .entries
                .iter()
                .find(|entry| safe_label_bytes(&entry.name) == "App.app")
                .unwrap()
                .can_open
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

    fn symlink_for_test(target: &Path, link: &Path) {
        unix_fs::symlink(target, link).unwrap();
    }
}
