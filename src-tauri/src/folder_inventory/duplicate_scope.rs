//! Conservative native path boundary checks for explicit content comparison.
//! Paths are temporary native observations; they never enter renderer state.
use std::{
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
fn protected_component(name: &[u8]) -> bool {
    super::is_managed_boundary(name)
        || [
            b"Library".as_slice(),
            b"System",
            b".git",
            b".ssh",
            b".gnupg",
            b".aws",
            b".azure",
            b".kube",
            b".config",
            b"Keychains",
            b"Secrets",
            b"Credentials",
            b"CloudStorage",
            b"Mobile Documents",
        ]
        .iter()
        .any(|word| name.eq_ignore_ascii_case(word))
}

pub(super) fn ensure_allowed(
    fd: &OwnedFd,
    started: Instant,
    duration: Duration,
) -> Result<(), &'static str> {
    #[cfg(target_os = "macos")]
    {
        use std::{
            ffi::OsStr,
            os::{fd::AsRawFd, unix::ffi::OsStrExt},
            path::{Component, Path},
        };
        let check = || {
            if started.elapsed() >= duration {
                Err("Content scope verification exceeded its cooperative time limit.")
            } else {
                Ok(())
            }
        };
        check()?;
        let before = rustix::fs::fstat(fd).map_err(|_| "The content scope cannot be verified.")?;
        let mut buffer = [0u8; libc::MAXPATHLEN as usize];
        // F_GETPATH writes at most MAXPATHLEN bytes to the supplied buffer (macOS SDK sys/fcntl.h).
        // This bounded, writable buffer lives across the syscall and the fd remains owned.
        let result = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
        check()?;
        if result < 0 {
            return Err("The native content scope path is unavailable.");
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("The native content scope path exceeds its supported limit.")?;
        let raw = Path::new(OsStr::from_bytes(&buffer[..end]));
        if !raw.is_absolute() {
            return Err("The native content scope path is unavailable.");
        }
        let canonical = std::fs::canonicalize(raw)
            .map_err(|_| "The native content scope path cannot be verified.")?;
        check()?;
        if canonical.components().any(|component| match component {
            Component::Normal(name) => protected_component(name.as_bytes()),
            _ => false,
        }) || [
            "/usr",
            "/bin",
            "/sbin",
            "/etc",
            "/private/etc",
            "/dev",
            "/proc",
            "/sys",
            "/Network",
        ]
        .iter()
        .any(|root| canonical.starts_with(root))
        {
            return Err("Content comparison is unavailable in a recognized managed, system, credential or cloud-store boundary.");
        }
        let named = rustix::fs::statat(
            rustix::fs::CWD,
            &canonical,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| "The native content scope changed.")?;
        let after = rustix::fs::fstat(fd).map_err(|_| "The native content scope changed.")?;
        check()?;
        if !super::root_metadata_matches(&before, &named)
            || !super::root_metadata_matches(&before, &after)
            || super::is_dataless(&after)
            || rustix::fs::FileType::from_raw_mode(after.st_mode) != rustix::fs::FileType::Directory
        {
            return Err("The native content scope changed.");
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (fd, started, duration);
        Err("Native content-scope verification is unsupported on this platform.")
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use rustix::fs::{open, Mode, OFlags};
    #[test]
    fn native_descriptor_path_refuses_protected_ancestors_even_through_an_alias() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let managed = temp.path().join("fixture.photoslibrary");
        let child = managed.join("ordinary");
        std::fs::create_dir_all(&child).unwrap();
        let alias = temp.path().join("alias");
        symlink(&child, &alias).unwrap();
        let fd = open(
            alias.join("."),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        assert!(ensure_allowed(&fd, Instant::now(), Duration::from_secs(5)).is_err());
        let ordinary = temp.path().join("ordinary");
        std::fs::create_dir(&ordinary).unwrap();
        let fd = open(
            &ordinary,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        ensure_allowed(&fd, Instant::now(), Duration::from_secs(5)).unwrap();
        assert!(ensure_allowed(&fd, Instant::now(), Duration::ZERO).is_err());
        for name in [
            b".ssh".as_slice(),
            b"KEYCHAINS",
            b"CloudStorage",
            b"Library",
        ] {
            assert!(protected_component(name));
        }
    }
}
