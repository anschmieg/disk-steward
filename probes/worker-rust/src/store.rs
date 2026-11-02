//! Private, app-created Git-worker store skeleton. This module deliberately
//! has no cleanup operation: an interrupted store is reconciliation-only.

use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum StoreError {
    #[allow(dead_code)]
    UnsupportedPlatform,
    Io(std::io::Error),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Rustix(rustix::io::Errno),
    InvalidIdentity,
    InvalidJournal,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => f.write_str("unsupported platform"),
            Self::Io(error) => write!(f, "store I/O: {error}"),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Rustix(error) => write!(f, "store filesystem operation: {error}"),
            Self::InvalidIdentity => f.write_str("store identity mismatch"),
            Self::InvalidJournal => f.write_str("store journal mismatch"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl From<rustix::io::Errno> for StoreError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Rustix(error)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreState {
    NeedsReconciliation,
}

/// Process-local handle for one newly created private store.
pub struct OwnedStore {
    parent_path: PathBuf,
    parent: File,
    root: File,
    name: String,
    parent_id: (u64, u64),
    root_id: (u64, u64),
    token: String,
}

impl OwnedStore {
    /// Creates a fresh private parent and store. There is intentionally no
    /// caller-supplied path and no TempDir/drop cleanup behavior.
    pub fn create() -> Result<Self, StoreError> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            return Err(StoreError::UnsupportedPlatform);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        create_supported()
    }

    pub fn inspect_state(&self) -> Result<StoreState, StoreError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use rustix::fs::{openat, OFlags};
            if identity(&self.parent)? != self.parent_id || identity(&self.root)? != self.root_id {
                return Err(StoreError::InvalidIdentity);
            }
            let parent = open_parent(&self.parent_path)?;
            if identity(&parent)? != self.parent_id {
                return Err(StoreError::InvalidIdentity);
            }
            let root = openat(
                &parent,
                &self.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?;
            if identity(&root)? != self.root_id {
                return Err(StoreError::InvalidIdentity);
            }
            validate_private_directory(&root)?;
            let marker = openat(
                &root,
                "owner.journal",
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )?;
            let mut bytes = Vec::new();
            let marker = File::from(marker);
            let metadata = marker.metadata()?;
            use std::os::unix::fs::MetadataExt;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.mode() & 0o777 != 0o600
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(StoreError::InvalidJournal);
            }
            marker.take(512).read_to_end(&mut bytes)?;
            let expected = format!(
                "disk-steward-worker-store-v1\n{}\n{}:{}\n{}:{}\ncreated\n",
                self.token, self.parent_id.0, self.parent_id.1, self.root_id.0, self.root_id.1
            );
            if bytes != expected.as_bytes() {
                return Err(StoreError::InvalidJournal);
            }
            Ok(StoreState::NeedsReconciliation)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(StoreError::UnsupportedPlatform)
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_supported() -> Result<OwnedStore, StoreError> {
    use rustix::fs::{mkdirat, openat, Mode, OFlags};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let parent_path = std::env::temp_dir().canonicalize()?;
    let parent = open_parent(&parent_path)?;
    let parent_id = identity(&parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidIdentity)?
        .as_nanos();
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let token = format!("{:x}-{:x}-{:x}", std::process::id(), nonce, serial);
    let name = format!("disk-steward-worker-{token}");
    mkdirat(&parent, &name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
    let root = openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    validate_private_directory(&root)?;
    let root_id = identity(&root)?;
    let marker = openat(
        &root,
        "owner.journal",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut marker = File::from(marker);
    writeln!(marker, "disk-steward-worker-store-v1")?;
    writeln!(marker, "{token}")?;
    writeln!(marker, "{}:{}", parent_id.0, parent_id.1)?;
    writeln!(marker, "{}:{}", root_id.0, root_id.1)?;
    writeln!(marker, "created")?;
    marker.sync_all()?;
    drop(marker);
    // Keep open descriptors as the authority; the path is retained only to
    // reacquire and compare the internally generated parent on reopen.
    File::from(root.try_clone()?).sync_all()?;
    File::from(parent.try_clone()?).sync_all()?;
    Ok(OwnedStore {
        parent_path,
        parent: File::from(parent),
        root: File::from(root),
        name,
        parent_id,
        root_id,
        token,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_private_directory(fd: &impl std::os::fd::AsFd) -> Result<(), StoreError> {
    let stat = rustix::fs::fstat(fd)?;
    if stat.st_mode & 0o777 != 0o700 || stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(StoreError::InvalidIdentity);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_parent(path: &std::path::Path) -> Result<std::os::fd::OwnedFd, StoreError> {
    use rustix::fs::{open, Mode, OFlags};
    Ok(open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn identity(fd: &impl std::os::fd::AsFd) -> Result<(u64, u64), StoreError> {
    let stat = rustix::fs::fstat(fd)?;
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fresh_store_inspects_only_as_reconciliation() {
        let store = OwnedStore::create().unwrap();
        assert!(!store.token().is_empty());
        assert_eq!(
            store.inspect_state().unwrap(),
            StoreState::NeedsReconciliation
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn changed_owner_journal_is_rejected() {
        use rustix::fs::{openat, OFlags};
        use std::io::Write;
        let store = OwnedStore::create().unwrap();
        let marker = openat(
            &store.root,
            "owner.journal",
            OFlags::WRONLY | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let mut marker = File::from(marker);
        marker.write_all(b"forged\n").unwrap();
        marker.sync_all().unwrap();
        assert!(matches!(
            store.inspect_state(),
            Err(StoreError::InvalidJournal)
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn another_created_root_cannot_satisfy_recorded_identity() {
        let mut first = OwnedStore::create().unwrap();
        let mut second = OwnedStore::create().unwrap();
        std::mem::swap(&mut first.name, &mut second.name);
        assert!(matches!(
            first.inspect_state(),
            Err(StoreError::InvalidIdentity)
        ));
        assert!(matches!(
            second.inspect_state(),
            Err(StoreError::InvalidIdentity)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn public_owner_journal_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let store = OwnedStore::create().unwrap();
        let path = store.parent_path.join(&store.name).join("owner.journal");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.inspect_state(),
            Err(StoreError::InvalidJournal)
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn symlinked_owner_journal_is_not_followed() {
        let store = OwnedStore::create().unwrap();
        let root = store.parent_path.join(&store.name);
        std::fs::rename(root.join("owner.journal"), root.join("saved.journal")).unwrap();
        std::os::unix::fs::symlink("saved.journal", root.join("owner.journal")).unwrap();
        assert!(store.inspect_state().is_err());
        assert!(std::fs::read(root.join("saved.journal"))
            .unwrap()
            .starts_with(b"disk-steward-worker-store-v1\n"));
    }
}
