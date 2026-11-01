//! Disposable fixture probe for directory-relative, no-overwrite renames.
//!
//! This is not production cleanup code. Callers supply only single-component
//! fixture names relative to already-open directory handles.

use std::fs::File;

#[derive(Debug, PartialEq, Eq)]
pub enum RenameState {
    Renamed,
    DestinationExists,
    Unsupported(String),
    Failed(String),
    /// A rename may have occurred, but identity/outcome verification did not
    /// establish its result. Callers must reconcile; never blindly retry.
    OutcomeUncertain(String),
}

/// Rejects a final symlink by opening it with O_NOFOLLOW, then revalidates the
/// observed identity after isolation. A source name can still be swapped after
/// this check and before rename; this probe does not claim to eliminate that
/// name-to-identity race.
pub fn isolate(source_dir: &File, source: &str, holding_dir: &File, holding: &str) -> RenameState {
    if !single_component(source) || !single_component(holding) {
        return RenameState::Failed("names must each be one normal path component".into());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{openat, FileType, Mode, OFlags};
        let before = match openat(
            source_dir,
            source,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(error) => return classify(error),
        };
        let before_stat = match rustix::fs::fstat(&before) {
            Ok(stat) => stat,
            Err(error) => return RenameState::Failed(error.to_string()),
        };
        let before_type = FileType::from_raw_mode(before_stat.st_mode);
        if !before_type.is_file() && !before_type.is_dir() {
            return RenameState::Failed("special files are not eligible for this probe".into());
        }

        match rustix::fs::renameat_with(
            source_dir,
            source,
            holding_dir,
            holding,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                // Re-open the isolated name with O_NOFOLLOW and compare the
                // actual moved object. This detects a pre-rename name swap;
                // it cannot undo it and production must reconcile it.
                let after = match openat(
                    holding_dir,
                    holding,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                ) {
                    Ok(fd) => fd,
                    Err(error) => {
                        return RenameState::OutcomeUncertain(format!(
                            "renamed, then revalidation failed: {error}"
                        ))
                    }
                };
                let after_stat = match rustix::fs::fstat(&after) {
                    Ok(stat) => stat,
                    Err(error) => {
                        return RenameState::OutcomeUncertain(format!(
                            "renamed, then fstat failed: {error}"
                        ))
                    }
                };
                revalidation_outcome(
                    before_stat.st_dev as i128,
                    before_stat.st_ino as i128,
                    after_stat.st_dev as i128,
                    after_stat.st_ino as i128,
                )
            }
            Err(error) => classify_rename(error),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (source_dir, source, holding_dir, holding);
        RenameState::Unsupported(format!(
            "atomic no-replace rename is not implemented for {}",
            std::env::consts::OS
        ))
    }
}

fn single_component(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let mut components = std::path::Path::new(name).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

// Only the unix rename implementation and the unix-gated tests call this;
// on other hosts neither is compiled.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn revalidation_outcome(
    before_dev: i128,
    before_ino: i128,
    after_dev: i128,
    after_ino: i128,
) -> RenameState {
    if before_dev == after_dev && before_ino == after_ino {
        RenameState::Renamed
    } else {
        RenameState::OutcomeUncertain(
            "rename was issued but isolated object identity differs; reconcile before retry".into(),
        )
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify(error: rustix::io::Errno) -> RenameState {
    use rustix::io::Errno;
    if error == Errno::EXIST || error == Errno::NOTEMPTY {
        RenameState::DestinationExists
    } else if error == Errno::NOSYS || error == Errno::OPNOTSUPP || error == Errno::INVAL {
        RenameState::Unsupported(error.to_string())
    } else {
        RenameState::Failed(std::io::Error::from(error).to_string())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify_rename(error: rustix::io::Errno) -> RenameState {
    use rustix::io::Errno;
    if error == Errno::EXIST || error == Errno::NOTEMPTY {
        RenameState::DestinationExists
    } else if error == Errno::NOSYS
        || error == Errno::OPNOTSUPP
        || error == Errno::INVAL
        || error == Errno::XDEV
    {
        RenameState::Unsupported(error.to_string())
    } else {
        RenameState::OutcomeUncertain(format!(
            "no-replace rename returned an unexpected error; reconcile before retry: {error}"
        ))
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    fn dirs() -> (tempfile::TempDir, File, File) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("source")).unwrap();
        fs::create_dir(temp.path().join("holding")).unwrap();
        let source = File::open(temp.path().join("source")).unwrap();
        let holding = File::open(temp.path().join("holding")).unwrap();
        (temp, source, holding)
    }

    #[test]
    fn occupied_destination_is_never_overwritten() {
        let src = tempfile::tempdir().unwrap();
        let from_dir = src.path().join("from");
        let to_dir = src.path().join("to");
        fs::create_dir(&from_dir).unwrap();
        fs::create_dir(&to_dir).unwrap();
        fs::write(from_dir.join("item"), b"source").unwrap();
        fs::write(to_dir.join("item"), b"destination sentinel").unwrap();
        let from = File::open(from_dir).unwrap();
        let to = File::open(to_dir).unwrap();
        assert_eq!(
            isolate(&from, "item", &to, "item"),
            RenameState::DestinationExists
        );
        assert_eq!(fs::read(src.path().join("from/item")).unwrap(), b"source");
        assert_eq!(
            fs::read(src.path().join("to/item")).unwrap(),
            b"destination sentinel"
        );
    }

    #[test]
    fn rename_and_restore_preserve_device_and_inode() {
        let (temp, source, holding) = dirs();
        fs::write(temp.path().join("source/item"), b"fixture").unwrap();
        let original = fs::metadata(temp.path().join("source/item")).unwrap();
        assert_eq!(
            isolate(&source, "item", &holding, "receipt"),
            RenameState::Renamed
        );
        let staged = fs::metadata(temp.path().join("holding/receipt")).unwrap();
        assert_eq!(
            (staged.dev(), staged.ino()),
            (original.dev(), original.ino())
        );
        assert_eq!(
            isolate(&holding, "receipt", &source, "item"),
            RenameState::Renamed
        );
        let restored = fs::metadata(temp.path().join("source/item")).unwrap();
        assert_eq!(
            (restored.dev(), restored.ino()),
            (original.dev(), original.ino())
        );
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_is_rejected_without_moving_link_or_target() {
        let (temp, source, holding) = dirs();
        fs::write(temp.path().join("source/target"), b"target").unwrap();
        std::os::unix::fs::symlink("target", temp.path().join("source/link")).unwrap();
        assert!(matches!(
            isolate(&source, "link", &holding, "receipt"),
            RenameState::Failed(_)
        ));
        assert!(fs::symlink_metadata(temp.path().join("source/link"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read(temp.path().join("source/target")).unwrap(),
            b"target"
        );
    }

    #[test]
    fn unsafe_names_are_rejected_before_mutation() {
        let (_temp, source, holding) = dirs();
        for name in ["../item", "link/", "link//", ".", "..", ""] {
            assert!(matches!(
                isolate(&source, name, &holding, "receipt"),
                RenameState::Failed(_)
            ));
        }
    }

    #[test]
    fn injected_post_rename_identity_mismatch_requires_reconciliation() {
        assert!(matches!(
            revalidation_outcome(10, 20, 10, 21),
            RenameState::OutcomeUncertain(_)
        ));
        assert_eq!(revalidation_outcome(10, 20, 10, 20), RenameState::Renamed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn special_fifo_is_rejected_without_blocking_or_moving() {
        let (temp, source, holding) = dirs();
        rustix::fs::mkfifoat(&source, "pipe", rustix::fs::Mode::from_bits_truncate(0o600)).unwrap();
        assert!(matches!(
            isolate(&source, "pipe", &holding, "receipt"),
            RenameState::Failed(_)
        ));
        assert!(temp.path().join("source/pipe").exists());
        assert!(!temp.path().join("holding/receipt").exists());
    }

    #[test]
    fn trailing_slash_cannot_follow_directory_symlink() {
        let (temp, source, holding) = dirs();
        fs::create_dir(temp.path().join("source/real-dir")).unwrap();
        std::os::unix::fs::symlink("real-dir", temp.path().join("source/dir-link")).unwrap();
        for name in ["dir-link/", "dir-link//"] {
            assert!(matches!(
                isolate(&source, name, &holding, "receipt"),
                RenameState::Failed(_)
            ));
        }
        assert!(fs::symlink_metadata(temp.path().join("source/dir-link"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unsupported_target_reports_state_without_mutating() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source-fixture");
        std::fs::write(&source, b"fixture sentinel").unwrap();
        let source_handle = File::open(&source).unwrap();
        let holding_handle = File::open(&source).unwrap();
        assert!(matches!(
            isolate(&source_handle, "item", &holding_handle, "receipt"),
            RenameState::Unsupported(_)
        ));
        assert_eq!(std::fs::read(source).unwrap(), b"fixture sentinel");
    }
}
