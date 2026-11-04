//! Native-only conservative policy for explicit subtree observations.
use rustix::fs::{fstat, openat, Mode, OFlags, Stat};
use std::{
    collections::BTreeSet,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

#[derive(PartialEq, Eq)]
pub(crate) struct SummaryExclusionPolicy {
    identities: BTreeSet<(u64, u64)>,
}
impl SummaryExclusionPolicy {
    pub(crate) fn from_identities(identities: BTreeSet<(u64, u64)>) -> Self {
        Self { identities }
    }
    pub(crate) fn excludes(&self, stat: &Stat) -> bool {
        self.identities.contains(&(stat.st_dev as u64, stat.st_ino))
    }
    pub(crate) fn ensure_ancestor_scope_allowed(
        &self,
        fd: &OwnedFd,
        started: Instant,
        duration: Duration,
    ) -> Result<(), &'static str> {
        if self.identities.is_empty() {
            return Ok(());
        }
        let check_time = || {
            if started.elapsed() >= duration {
                Err("Remembered exclusions could not be checked within the observation limit.")
            } else {
                Ok(())
            }
        };
        check_time()?;
        let mut current = openat(
            fd,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "The selected scope's exclusion ancestry is unavailable.")?;
        check_time()?;
        for _ in 0..128 {
            check_time()?;
            let before = fstat(&current)
                .map_err(|_| "The selected scope's exclusion ancestry is unavailable.")?;
            check_time()?;
            if self.excludes(&before) {
                return Err("The selected scope is inside a remembered exclusion. Remove that rule separately before inspecting it.");
            }
            let parent = openat(
                &current,
                "..",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "The selected scope's exclusion ancestry is unavailable.")?;
            check_time()?;
            let parent_stat = fstat(&parent)
                .map_err(|_| "The selected scope's exclusion ancestry is unavailable.")?;
            check_time()?;
            if before.st_dev == parent_stat.st_dev && before.st_ino == parent_stat.st_ino {
                return Ok(());
            }
            current = parent;
        }
        Err("The selected scope's exclusion ancestry exceeds the supported depth.")
    }
    #[cfg(test)]
    pub(crate) fn empty_for_tests() -> Self {
        Self::for_tests(Vec::new())
    }
    #[cfg(test)]
    pub(crate) fn for_tests(identities: Vec<(u64, u64)>) -> Self {
        Self::from_identities(identities.into_iter().collect())
    }
}
