use super::PrivateRoot;
use std::{
    fs::{self, File},
    io::{self, Write},
    os::unix::fs::{FileExt, MetadataExt},
    path::PathBuf,
};

use verification_budget::{BudgetController, JobId, Limits};
use verification_store_registry::VerificationStoreRegistry;

const NAME: &str = "pack.download";
pub(super) const REGISTRY_NAME: &str = "pack-budget.sqlite";
pub(super) const STORE_ID: &str = "0123456789abcdef0123456789abcdef";
pub(super) const RESERVATION: u64 = 128 * 1024;
const MAX_LIMIT: u64 = 512;
const HEADROOM: u64 = 1024 * 1024;

pub(super) struct BudgetFileSink {
    root_path: PathBuf,
    root_fd: File,
    root_identity: (u64, u64, u32, u32),
    path: PathBuf,
    file: File,
    file_identity: (u64, u64),
    logical_limit: u64,
    high_water: u64,
    budget: BudgetController,
    job: JobId,
    registry: VerificationStoreRegistry,
    reservation_bytes: u64,
    signal: crate::CancellationHandle,
}

impl BudgetFileSink {
    pub(super) fn new(root: &PrivateRoot, signal: crate::CancellationHandle) -> io::Result<Self> {
        Self::new_with_limit(root, signal, MAX_LIMIT)
    }

    pub(super) fn new_with_limit(
        root: &PrivateRoot,
        signal: crate::CancellationHandle,
        logical_limit: u64,
    ) -> io::Result<Self> {
        if logical_limit == 0 || logical_limit > MAX_LIMIT {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "pack sink limit invalid",
            ));
        }
        let root_meta = fs::symlink_metadata(&root.path)?;
        if !root.identity_matches()?
            || !root_meta.file_type().is_dir()
            || root_meta.mode() & 0o7777 != 0o700
        {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "pack sink root identity invalid",
            ));
        }
        let root_identity = (
            root_meta.dev(),
            root_meta.ino(),
            root_meta.uid(),
            root_meta.mode() & 0o7777,
        );
        let root_fd = File::from(
            rustix::fs::open(
                &root.path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| {
                safe_error(
                    io::ErrorKind::PermissionDenied,
                    "pack sink root open failed",
                )
            })?,
        );
        let opened_root = root_fd.metadata()?;
        if (
            opened_root.dev(),
            opened_root.ino(),
            opened_root.uid(),
            opened_root.mode() & 0o7777,
        ) != root_identity
        {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "pack sink root changed before open",
            ));
        }
        let free = free_bytes(&root_fd)?;
        let mut budget = BudgetController::new(Limits {
            headroom_bytes: HEADROOM,
        });
        let job = budget
            .start(free)
            .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink reserve unavailable"))?;
        budget
            .check_store_admission(job, logical_limit, free)
            .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink admission refused"))?;

        // Commit durable admission before creating the payload file.
        let registry_admission = (|| {
            let mut registry = VerificationStoreRegistry::open(root.path.join(REGISTRY_NAME))
                .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink registry unavailable"))?;
            let parent = root.path.parent().ok_or_else(|| {
                safe_error(io::ErrorKind::PermissionDenied, "pack sink parent missing")
            })?;
            let parent_meta = fs::symlink_metadata(parent)?;
            registry
                .register(
                    STORE_ID,
                    parent_meta.dev(),
                    parent_meta.ino(),
                    opened_root.dev(),
                    opened_root.ino(),
                )
                .map_err(|_| {
                    safe_error(
                        io::ErrorKind::Other,
                        "pack sink registry registration refused",
                    )
                })?;
            let reservation = registry
                .reserve(STORE_ID, 0, RESERVATION)
                .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink reservation refused"))?;
            if reservation.reserved_bytes != RESERVATION {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "pack sink reservation mismatch",
                ));
            }
            Ok((registry, reservation.reserved_bytes))
        })();
        let (registry, reservation_bytes) = match registry_admission {
            Ok(admission) => admission,
            Err(error) => {
                signal.cancel();
                return Err(error);
            }
        };

        // Registry creation consumed control metadata space; sample again before payload creation.
        let free_after_registry = free_bytes(&root_fd)?;
        budget
            .check_store_admission(job, logical_limit, free_after_registry)
            .map_err(|_| {
                safe_error(
                    io::ErrorKind::Other,
                    "pack sink post-registry reserve refused",
                )
            })?;
        let path = root.path.join(NAME);
        let file = File::from(
            rustix::fs::openat(
                &root_fd,
                NAME,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_bits(0o600).unwrap(),
            )
            .map_err(|_| safe_error(io::ErrorKind::PermissionDenied, "pack sink create refused"))?,
        );
        let file_meta = file.metadata()?;
        let file_identity = (file_meta.dev(), file_meta.ino());
        let sink = Self {
            root_path: root.path.clone(),
            root_fd,
            root_identity,
            path,
            file,
            file_identity,
            logical_limit,
            high_water: 0,
            budget,
            job,
            registry,
            reservation_bytes,
            signal,
        };
        sink.validate_identity()?;
        Ok(sink)
    }

    pub(super) fn bytes_charged(&self) -> u64 {
        self.budget
            .snapshot(self.job)
            .map(|snapshot| snapshot.usage.stored_bytes)
            .expect("created budget job remains registered")
    }

    pub(super) fn reservation_bytes(&self) -> io::Result<u64> {
        let record = self
            .registry
            .get(STORE_ID)
            .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink reservation lookup failed"))?
            .ok_or_else(|| safe_error(io::ErrorKind::NotFound, "pack sink reservation missing"))?;
        Ok(record.reserved_bytes)
    }

    pub(super) fn contents(&self) -> io::Result<Vec<u8>> {
        self.validate_identity()?;
        let length = self.file.metadata()?.len();
        if length > self.logical_limit {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "pack sink contents exceed limit",
            ));
        }
        let mut contents = vec![
            0;
            usize::try_from(length).map_err(|_| {
                safe_error(
                    io::ErrorKind::InvalidData,
                    "pack sink contents size invalid",
                )
            })?
        ];
        let mut offset = 0usize;
        while offset < contents.len() {
            let read = self.file.read_at(&mut contents[offset..], offset as u64)?;
            if read == 0 {
                return Err(safe_error(
                    io::ErrorKind::UnexpectedEof,
                    "pack sink contents changed",
                ));
            }
            offset += read;
        }
        self.validate_identity()?;
        Ok(contents)
    }

    pub(super) fn sync_and_observe(&mut self) -> io::Result<()> {
        if let Err(error) = self.validate_identity() {
            self.signal.cancel();
            return Err(error);
        }
        let sync_result = self.file.sync_all();
        let observed = self.observe_after_operation();
        if let Err(error) = sync_result {
            self.signal.cancel();
            return Err(safe_error(error.kind(), "pack sink sync failed"));
        }
        observed
    }

    fn validate_identity(&self) -> io::Result<()> {
        let record = self
            .registry
            .get(STORE_ID)
            .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink registry validation failed"))?
            .ok_or_else(|| safe_error(io::ErrorKind::NotFound, "pack sink reservation missing"))?;
        if record.reserved_bytes != self.reservation_bytes
            || (record.root_dev, record.root_ino) != (self.root_identity.0, self.root_identity.1)
            || !matches!(
                record.state,
                verification_store_registry::StoreState::Registered
                    | verification_store_registry::StoreState::Active
            )
        {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "pack sink reservation binding changed",
            ));
        }
        let root = fs::symlink_metadata(&self.root_path)?;
        let held_root = self.root_fd.metadata()?;
        if !root.is_dir()
            || root.file_type().is_symlink()
            || (root.dev(), root.ino(), root.uid(), root.mode() & 0o7777) != self.root_identity
            || (
                held_root.dev(),
                held_root.ino(),
                held_root.uid(),
                held_root.mode() & 0o7777,
            ) != self.root_identity
        {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "pack sink root identity changed",
            ));
        }
        let held = self.file.metadata()?;
        let named = fs::symlink_metadata(&self.path)?;
        if !held.is_file()
            || !named.is_file()
            || named.file_type().is_symlink()
            || (held.dev(), held.ino()) != self.file_identity
            || (named.dev(), named.ino()) != self.file_identity
            || held.uid() != self.root_identity.2
            || named.uid() != self.root_identity.2
            || held.mode() & 0o7777 != 0o600
            || named.mode() & 0o7777 != 0o600
            || held.nlink() != 1
            || named.nlink() != 1
        {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "pack sink file identity changed",
            ));
        }
        Ok(())
    }

    fn free_bytes(&self) -> io::Result<u64> {
        free_bytes(&self.root_fd)
    }

    fn observed_high_water(&self) -> io::Result<(u64, u64)> {
        self.validate_identity()?;
        let metadata = self.file.metadata()?;
        let allocated = metadata.blocks().checked_mul(512).ok_or_else(|| {
            safe_error(io::ErrorKind::InvalidData, "pack sink allocation overflow")
        })?;
        Ok((metadata.len(), metadata.len().max(allocated)))
    }

    fn charge_high_water(&mut self, observed: u64, free: u64) -> io::Result<()> {
        let high_water = self.high_water.max(observed);
        let growth = high_water
            .checked_sub(self.high_water)
            .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "pack sink charge overflow"))?;
        self.high_water = high_water;
        self.budget
            .record_store(self.job, growth, free)
            .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink storage budget blocked"))?;
        if high_water > self.reservation_bytes {
            self.signal.cancel();
            return Err(safe_error(
                io::ErrorKind::Other,
                "pack sink exceeded durable reservation",
            ));
        }
        Ok(())
    }

    fn observe_after_operation(&mut self) -> io::Result<()> {
        let observed = self.observed_high_water();
        let free = self.free_bytes();
        let charge = match observed {
            Ok((_, high_water)) => {
                self.charge_high_water(high_water, free.as_ref().copied().unwrap_or(0))
            }
            Err(error) => Err(error),
        };
        if free.is_err() || charge.is_err() {
            self.signal.cancel();
        }
        free?;
        charge
    }
}

fn free_bytes(root: &impl std::os::fd::AsFd) -> io::Result<u64> {
    let stats = rustix::fs::fstatvfs(root)
        .map_err(|_| safe_error(io::ErrorKind::Other, "pack sink volume query failed"))?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidData, "pack sink free space overflow"))
}

impl Write for BudgetFileSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.signal.check().and_then(|_| self.validate_identity()) {
            self.signal.cancel();
            return Err(error);
        }
        if bytes.is_empty() {
            return Ok(0);
        }

        let (logical, current_high_water) = match self.observed_high_water() {
            Ok(observed) => observed,
            Err(error) => {
                self.signal.cancel();
                return Err(error);
            }
        };
        let free_before = match self.free_bytes() {
            Ok(free) => free,
            Err(error) => {
                self.signal.cancel();
                return Err(error);
            }
        };
        if let Err(error) = self.charge_high_water(current_high_water, free_before) {
            self.signal.cancel();
            return Err(error);
        }
        let remaining = self.logical_limit.saturating_sub(logical);
        let allowed = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if allowed == 0 {
            self.signal.cancel();
            return Ok(0);
        }
        if self
            .budget
            .check_store_admission(self.job, allowed as u64, free_before)
            .is_err()
        {
            self.signal.cancel();
            return Err(safe_error(
                io::ErrorKind::Other,
                "pack sink storage admission refused",
            ));
        }

        let write_result = self.file.write(&bytes[..allowed]);
        let observation_result = self.observe_after_operation();
        match write_result {
            Ok(count) if count <= allowed => {
                if count == 0 || observation_result.is_err() {
                    // Preserve Write's accepted-byte count; the transfer checks
                    // the cancellation signal immediately after the write.
                    self.signal.cancel();
                }
                Ok(count)
            }
            Ok(_) => {
                self.signal.cancel();
                Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "pack sink returned invalid byte count",
                ))
            }
            Err(error) => {
                self.signal.cancel();
                let _ = observation_result;
                Err(safe_error(error.kind(), "pack sink write failed"))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Err(error) = self.validate_identity() {
            self.signal.cancel();
            return Err(error);
        }
        let flush_result = self.file.flush();
        let observation_result = self.observe_after_operation();
        if let Err(error) = flush_result {
            self.signal.cancel();
            return Err(safe_error(error.kind(), "pack sink flush failed"));
        }
        observation_result
    }
}

fn safe_error(kind: io::ErrorKind, message: &'static str) -> io::Error {
    io::Error::new(kind, message)
}
