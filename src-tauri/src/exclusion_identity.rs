//! Exact directory identity observations for the private exclusion registry.
//!
//! This adapter records a lossless locator plus volume and opened-directory
//! identity. It does not traverse the selected directory, open its children,
//! create a bookmark, or grant future access.

use std::path::Path;

const MAX_LOCATOR_BYTES: usize = 4_096;
const MAX_BASENAME_CHARS: usize = 120;

/// Private observation passed only between native registry code and this
/// adapter. It deliberately has no serialization or debug representation.
pub(crate) struct BindingObservation {
    pub(crate) locator_bytes: Vec<u8>,
    pub(crate) volume_identity: Vec<u8>,
    pub(crate) directory_identity: Vec<u8>,
    pub(crate) basename: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdentityError {
    InvalidSelection,
    Missing,
    Unavailable,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Revalidation {
    Matched,
    Stale,
    Unavailable,
}

/// Observes one directory entry without following its leaf symlink. On macOS,
/// the volume UUID is read through Foundation and object identity comes from
/// the opened directory's device, inode, and creation time.
pub(crate) fn observe_directory(path: &Path) -> Result<BindingObservation, IdentityError> {
    #[cfg(target_os = "macos")]
    {
        observe_macos(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(IdentityError::Unsupported)
    }
}

/// Re-observes the exact locator. Unsupported or inaccessible metadata remains
/// unavailable; missing, replaced, renamed, or non-directory leaves are stale.
pub(crate) fn revalidate_directory(path: &Path, expected: &BindingObservation) -> Revalidation {
    match observe_directory(path) {
        Ok(current)
            if current.locator_bytes == expected.locator_bytes
                && current.volume_identity == expected.volume_identity
                && current.directory_identity == expected.directory_identity =>
        {
            Revalidation::Matched
        }
        Ok(_) => Revalidation::Stale,
        Err(IdentityError::InvalidSelection | IdentityError::Missing) => Revalidation::Stale,
        Err(IdentityError::Unavailable | IdentityError::Unsupported) => Revalidation::Unavailable,
    }
}

#[cfg(target_os = "macos")]
fn observe_macos(path: &Path) -> Result<BindingObservation, IdentityError> {
    use std::{
        fs::{self, File, OpenOptions},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };

    if !path.is_absolute() {
        return Err(IdentityError::InvalidSelection);
    }
    let locator = raw_locator_bytes(path)?;

    let before = fs::symlink_metadata(path).map_err(map_io_error)?;
    if !before.file_type().is_dir() {
        return Err(IdentityError::InvalidSelection);
    }

    // O_NOFOLLOW applies to the selected leaf; O_DIRECTORY rejects files.
    // The handle is only queried for metadata and is never enumerated.
    let directory: File = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(map_io_error)?;
    let opened = directory
        .metadata()
        .map_err(|_| IdentityError::Unavailable)?;
    if !opened.file_type().is_dir() || !same_object(&before, &opened) {
        return Err(IdentityError::InvalidSelection);
    }

    let (birth_seconds, birth_nanos) = birth_components(opened.created().ok())?;
    let volume_identity = volume_uuid_from_foundation(path)?;

    // Detect replacement during the Foundation path query. Both observations
    // must still identify the object held by the no-follow descriptor.
    let after = fs::symlink_metadata(path).map_err(map_io_error)?;
    if !after.file_type().is_dir() || !same_object(&opened, &after) {
        return Err(IdentityError::InvalidSelection);
    }

    let basename = sanitized_basename(path)?;
    // The private store wraps binding records in its own versioned encoding;
    // this field stays raw so native confirmation can reopen the exact path.
    let locator_bytes = locator;

    let mut directory_identity = Vec::with_capacity(36);
    directory_identity.extend_from_slice(b"DSXDIR\x01");
    directory_identity.extend_from_slice(&opened.dev().to_le_bytes());
    directory_identity.extend_from_slice(&opened.ino().to_le_bytes());
    directory_identity.extend_from_slice(&birth_seconds.to_le_bytes());
    directory_identity.extend_from_slice(&birth_nanos.to_le_bytes());

    Ok(BindingObservation {
        locator_bytes,
        volume_identity,
        directory_identity,
        basename,
    })
}

#[cfg(target_os = "macos")]
fn raw_locator_bytes(path: &Path) -> Result<Vec<u8>, IdentityError> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.len() > MAX_LOCATOR_BYTES
        || bytes.contains(&0)
    {
        return Err(IdentityError::InvalidSelection);
    }
    Ok(bytes.to_vec())
}

#[cfg(target_os = "macos")]
fn same_object(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    let left_birth = left.created().ok().and_then(|time| {
        time.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
    });
    let right_birth = right.created().ok().and_then(|time| {
        time.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
    });
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left_birth.is_some()
        && left_birth == right_birth
}

#[cfg(target_os = "macos")]
fn birth_components(created: Option<std::time::SystemTime>) -> Result<(u64, u32), IdentityError> {
    created
        .ok_or(IdentityError::Unavailable)?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
        .map_err(|_| IdentityError::Unavailable)
}

#[cfg(target_os = "macos")]
fn sanitized_basename(path: &Path) -> Result<String, IdentityError> {
    use std::os::unix::ffi::OsStrExt;

    let name = path
        .file_name()
        .ok_or(IdentityError::InvalidSelection)?
        .as_bytes();
    if name.is_empty() || name == b"." || name == b".." {
        return Err(IdentityError::InvalidSelection);
    }
    let mut visible: Vec<char> = String::from_utf8_lossy(name)
        .chars()
        .flat_map(|character| {
            if crate::selected_metadata::is_bidi_control(character) {
                format!("\\u{{{:04X}}}", character as u32)
                    .chars()
                    .collect::<Vec<_>>()
            } else if character.is_control() {
                Vec::new()
            } else {
                vec![character]
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
                bounded.extend(format!("\\u{{{:04X}}}", character as u32).chars());
            } else {
                bounded.push(character);
            }
        }
        visible = bounded;
    }
    let sanitized: String = visible.into_iter().take(MAX_BASENAME_CHARS).collect();
    if sanitized.trim().is_empty() {
        Err(IdentityError::InvalidSelection)
    } else {
        Ok(sanitized)
    }
}

#[cfg(target_os = "macos")]
fn map_io_error(error: std::io::Error) -> IdentityError {
    match error.kind() {
        std::io::ErrorKind::NotFound => IdentityError::Missing,
        std::io::ErrorKind::InvalidInput => IdentityError::InvalidSelection,
        _ => IdentityError::Unavailable,
    }
}

#[cfg(target_os = "macos")]
fn volume_uuid_from_foundation(path: &Path) -> Result<Vec<u8>, IdentityError> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, ptr::NonNull};

    use objc2::rc::autoreleasepool;
    use objc2_foundation::{
        NSArray, NSNumber, NSString, NSURLVolumeIsLocalKey, NSURLVolumeUUIDStringKey, NSURL,
    };

    let filesystem_path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| IdentityError::InvalidSelection)?;
    let pointer =
        NonNull::new(filesystem_path.as_ptr().cast_mut()).ok_or(IdentityError::InvalidSelection)?;
    // SAFETY: `filesystem_path` owns a NUL-terminated filesystem path and
    // remains alive for this call. Its bytes came directly from the absolute
    // path's Unix representation, with embedded NULs rejected by CString.
    let url = unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(pointer, true, None)
    };
    // SAFETY: Foundation exports this process-lifetime immutable resource key.
    let (volume_uuid_key, volume_local_key) =
        unsafe { (NSURLVolumeUUIDStringKey, NSURLVolumeIsLocalKey) };
    let keys = NSArray::from_slice(&[volume_uuid_key, volume_local_key]);
    let values = url
        .resourceValuesForKeys_error(&keys)
        .map_err(|_| IdentityError::Unavailable)?;
    let is_local = values
        .objectForKey(volume_local_key)
        .and_then(|value| value.downcast_ref::<NSNumber>().map(NSNumber::boolValue));
    require_local_volume(is_local)?;
    let value = values
        .objectForKey(volume_uuid_key)
        .ok_or(IdentityError::Unavailable)?;
    let uuid = value
        .downcast_ref::<NSString>()
        .ok_or(IdentityError::Unavailable)?;
    let uuid = autoreleasepool(|_| uuid.to_string());
    if !valid_uuid(&uuid) {
        return Err(IdentityError::Unavailable);
    }

    volume_identity_bytes(&uuid)
}

#[cfg(target_os = "macos")]
fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(target_os = "macos")]
fn volume_identity_bytes(value: &str) -> Result<Vec<u8>, IdentityError> {
    if !valid_uuid(value) {
        return Err(IdentityError::Unavailable);
    }
    let mut bytes = b"DSXVOL\x01".to_vec();
    bytes.extend(value.to_ascii_lowercase().as_bytes());
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn require_local_volume(value: Option<bool>) -> Result<(), IdentityError> {
    if value == Some(true) {
        Ok(())
    } else {
        Err(IdentityError::Unavailable)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt};

    fn observe(path: &Path) -> BindingObservation {
        match observe_directory(path) {
            Ok(observation) => observation,
            Err(_) => panic!("disposable directory identity should be supported"),
        }
    }

    #[test]
    fn preserves_unicode_and_raw_non_utf8_locator_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let unicode = fixture.path().join("München 📁");
        fs::create_dir(&unicode).unwrap();
        let unicode_observation = observe(&unicode);
        assert!(unicode_observation.basename.contains("München"));
        assert_eq!(
            unicode_observation.locator_bytes,
            unicode.as_os_str().as_encoded_bytes()
        );

        let non_utf8_name = OsString::from_vec(b"directory-\xff".to_vec());
        let non_utf8 = fixture.path().join(non_utf8_name);
        let locator = raw_locator_bytes(&non_utf8).unwrap();
        assert_eq!(locator, non_utf8.as_os_str().as_encoded_bytes());
        assert!(sanitized_basename(&non_utf8).unwrap().contains('�'));
    }

    #[test]
    fn matches_only_the_same_exact_directory_and_detects_replacement_and_move() {
        let fixture = tempfile::tempdir().unwrap();
        let original = fixture.path().join("selected");
        fs::create_dir(&original).unwrap();
        let sentinel = original.join("untouched");
        fs::write(&sentinel, b"sentinel bytes").unwrap();
        let binding = observe(&original);
        assert_eq!(
            revalidate_directory(&original, &binding),
            Revalidation::Matched
        );
        assert_eq!(fs::read(sentinel).unwrap(), b"sentinel bytes");

        let moved = fixture.path().join("moved");
        fs::rename(&original, &moved).unwrap();
        assert_eq!(
            revalidate_directory(&original, &binding),
            Revalidation::Stale
        );
        assert_eq!(revalidate_directory(&moved, &binding), Revalidation::Stale);

        fs::create_dir(&original).unwrap();
        assert_eq!(
            revalidate_directory(&original, &binding),
            Revalidation::Stale
        );
    }

    #[test]
    fn rejects_symlink_files_and_missing_paths() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("real");
        let link = fixture.path().join("link");
        let file = fixture.path().join("file");
        fs::create_dir(&directory).unwrap();
        fs::write(&file, b"fixture").unwrap();
        symlink(&directory, &link).unwrap();

        assert!(matches!(
            observe_directory(&link),
            Err(IdentityError::InvalidSelection)
        ));
        assert!(matches!(
            observe_directory(&file),
            Err(IdentityError::InvalidSelection)
        ));
        assert_eq!(
            revalidate_directory(&fixture.path().join("missing"), &observe(&directory)),
            Revalidation::Stale
        );
    }

    #[test]
    fn birth_time_and_volume_identity_are_present_and_versioned() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("identity");
        fs::create_dir(&directory).unwrap();
        let observation = observe(&directory);
        assert!(observation.volume_identity.starts_with(b"DSXVOL\x01"));
        assert_eq!(observation.volume_identity.len(), 7 + 36);
        assert!(observation.directory_identity.starts_with(b"DSXDIR\x01"));
        assert_eq!(observation.directory_identity.len(), 7 + 8 + 8 + 8 + 4);
    }

    #[test]
    fn missing_birth_time_or_invalid_volume_uuid_fails_closed() {
        assert!(matches!(
            birth_components(None),
            Err(IdentityError::Unavailable)
        ));
        assert!(matches!(
            volume_identity_bytes("no-volume-identity"),
            Err(IdentityError::Unavailable)
        ));
    }

    #[test]
    fn absent_or_remote_volume_locality_fails_closed() {
        assert!(matches!(
            require_local_volume(None),
            Err(IdentityError::Unavailable)
        ));
        assert!(matches!(
            require_local_volume(Some(false)),
            Err(IdentityError::Unavailable)
        ));
        assert!(require_local_volume(Some(true)).is_ok());
    }

    #[test]
    fn escapes_edge_whitespace_and_bidi_controls_in_visible_basename() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join(" leading\u{202e}name ");
        let display = sanitized_basename(&path).unwrap();
        assert!(display.starts_with("\\u{0020}"));
        assert!(display.ends_with("\\u{0020}"));
        assert!(display.contains("\\u{202E}"));
        assert!(!display.chars().any(char::is_control));
        assert_eq!(display.trim(), display);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_platform_fails_closed() {
        assert!(matches!(
            observe_directory(Path::new("/")),
            Err(IdentityError::Unsupported)
        ));
    }
}
