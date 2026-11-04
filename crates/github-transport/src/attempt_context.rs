use crate::RepositoryUrl;
use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const MAX_AGE: Duration = Duration::from_secs(15 * 60);
const STATUS_VALID: u8 = 0;
const STATUS_EXPIRED: u8 = 1;
const STATUS_INVALID: u8 = 2;

#[derive(Clone, Debug)]
pub(crate) struct AttemptContext {
    repository: RepositoryUrl,
    started_at: Instant,
    started_wall: SystemTime,
    status: Arc<AtomicU8>,
}

impl PartialEq for AttemptContext {
    fn eq(&self, other: &Self) -> bool {
        self.repository == other.repository
            && self.started_at == other.started_at
            && self.started_wall == other.started_wall
            && Arc::ptr_eq(&self.status, &other.status)
    }
}

impl Eq for AttemptContext {}

impl AttemptContext {
    pub(crate) fn new(
        repository: RepositoryUrl,
        started_at: Instant,
        started_wall: SystemTime,
    ) -> Self {
        Self {
            repository,
            started_at,
            started_wall,
            status: Arc::new(AtomicU8::new(STATUS_VALID)),
        }
    }

    pub(crate) fn repository(&self) -> &RepositoryUrl {
        &self.repository
    }

    /// Checks endpoint and clock freshness; this is not durable, suspend-platform,
    /// or clock-tampering proof.
    pub(crate) fn check(&self, expected: &RepositoryUrl) -> io::Result<()> {
        self.check_at(expected, Instant::now(), SystemTime::now())
    }

    fn check_at(&self, expected: &RepositoryUrl, now: Instant, wall: SystemTime) -> io::Result<()> {
        if self.repository != *expected {
            return Err(safe_error(
                io::ErrorKind::InvalidInput,
                "verification repository mismatch",
            ));
        }

        if let Some(error) = status_error(self.status.load(Ordering::Acquire)) {
            return Err(error);
        }

        let age = match now.checked_duration_since(self.started_at) {
            Some(age) => age,
            None => return Err(self.latch_terminal(STATUS_INVALID)),
        };
        let wall_age = match wall.duration_since(self.started_wall) {
            Ok(age) => age,
            Err(_) => return Err(self.latch_terminal(STATUS_INVALID)),
        };
        if age >= MAX_AGE || wall_age >= MAX_AGE {
            return Err(self.latch_terminal(STATUS_EXPIRED));
        }
        if let Some(error) = status_error(self.status.load(Ordering::Acquire)) {
            return Err(error);
        }
        Ok(())
    }

    fn latch_terminal(&self, proposed: u8) -> io::Error {
        let terminal = match self.status.compare_exchange(
            STATUS_VALID,
            proposed,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => proposed,
            Err(first) => first,
        };
        match terminal {
            STATUS_EXPIRED => safe_error(io::ErrorKind::TimedOut, "verification context expired"),
            _ => safe_error(
                io::ErrorKind::InvalidData,
                "verification clock context invalid",
            ),
        }
    }
}

fn status_error(status: u8) -> Option<io::Error> {
    match status {
        STATUS_VALID => None,
        STATUS_EXPIRED => Some(safe_error(
            io::ErrorKind::TimedOut,
            "verification context expired",
        )),
        _ => Some(safe_error(
            io::ErrorKind::InvalidData,
            "verification clock context invalid",
        )),
    }
}

fn safe_error(kind: io::ErrorKind, message: &'static str) -> io::Error {
    io::Error::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(url: &str) -> RepositoryUrl {
        RepositoryUrl::parse(url).unwrap()
    }

    fn wall(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn validity_ends_at_the_exact_expiry_boundary() {
        let start = Instant::now();
        let start_wall = wall(10_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let expected = repository("https://github.com/owner/repo");

        assert!(context
            .check_at(
                &expected,
                start + MAX_AGE - Duration::from_nanos(1),
                start_wall + MAX_AGE - Duration::from_nanos(1),
            )
            .is_ok());
        let error = context
            .check_at(&expected, start + MAX_AGE, start_wall + MAX_AGE)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "verification context expired");
    }

    #[test]
    fn either_clock_can_expire_the_context() {
        for wall_expires in [false, true] {
            let start = Instant::now();
            let start_wall = wall(if wall_expires { 21_000 } else { 20_000 });
            let context = AttemptContext::new(
                repository("https://github.com/owner/repo"),
                start,
                start_wall,
            );
            let expected = repository("https://github.com/owner/repo");
            let (now, wall_now) = if wall_expires {
                (start + Duration::from_secs(1), start_wall + MAX_AGE)
            } else {
                (start + MAX_AGE, start_wall + Duration::from_secs(1))
            };
            let error = context.check_at(&expected, now, wall_now).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string(), "verification context expired");
        }
    }

    #[test]
    fn an_earlier_clock_instant_is_rejected() {
        for wall_reverses in [false, true] {
            let start = Instant::now();
            let start_wall = wall(30_000);
            let context = AttemptContext::new(
                repository("https://github.com/owner/repo"),
                start,
                start_wall,
            );
            let (now, wall_now) = if wall_reverses {
                (start, wall(29_999))
            } else {
                (
                    start.checked_sub(Duration::from_nanos(1)).unwrap(),
                    wall(30_001),
                )
            };
            let error = context
                .check_at(context.repository(), now, wall_now)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "verification clock context invalid");
        }
    }

    #[test]
    fn repository_identity_is_exact() {
        let original = repository("https://github.com/owner/repo");
        let context = AttemptContext::new(original, Instant::now(), wall(40_000));
        for url in [
            "https://github.com/other/repo",
            "https://github.com/Owner/repo",
            "https://github.com/owner/Repo",
            "https://github.com/owner/repo.git",
        ] {
            let error = context
                .check_at(&repository(url), Instant::now(), wall(40_001))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(error.to_string(), "verification repository mismatch");
        }
    }

    #[test]
    fn cloning_does_not_extend_the_context_lifetime() {
        let start = Instant::now();
        let start_wall = wall(50_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let cloned = context.clone();
        let expected = repository("https://github.com/owner/repo");
        let expiry = start + MAX_AGE;

        assert_eq!(context, cloned);
        for copy in [&context, &cloned] {
            let error = copy
                .check_at(&expected, expiry, start_wall + MAX_AGE)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string(), "verification context expired");
        }
    }

    #[test]
    fn terminal_expiry_survives_wall_clock_correction() {
        let start = Instant::now();
        let start_wall = wall(60_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let expected = repository("https://github.com/owner/repo");
        assert_eq!(
            context
                .check_at(
                    &expected,
                    start + Duration::from_secs(1),
                    start_wall + MAX_AGE
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let error = context
            .check_at(
                &expected,
                start + Duration::from_secs(2),
                start_wall + Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "verification context expired");
    }

    #[test]
    fn terminal_invalid_clock_survives_fresh_reading() {
        let start = Instant::now();
        let start_wall = wall(70_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let expected = repository("https://github.com/owner/repo");
        assert_eq!(
            context
                .check_at(&expected, start, wall(69_999))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let error = context
            .check_at(
                &expected,
                start + Duration::from_secs(1),
                start_wall + Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "verification clock context invalid");
    }

    #[test]
    fn clones_share_terminal_status_but_wrong_repository_does_not_poison_it() {
        let start = Instant::now();
        let start_wall = wall(80_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let clone_before = context.clone();
        let expected = repository("https://github.com/owner/repo");
        let wrong = repository("https://github.com/owner/other");

        assert_eq!(
            context
                .check_at(&wrong, start, start_wall)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(context.check_at(&expected, start, start_wall).is_ok());

        let error = clone_before
            .check_at(&expected, start + MAX_AGE, start_wall + MAX_AGE)
            .unwrap_err();
        let clone_after = context.clone();
        assert_eq!(context, clone_before);
        assert_eq!(clone_before, clone_after);
        assert_eq!(
            clone_after
                .check_at(
                    &expected,
                    start + Duration::from_secs(1),
                    start_wall + Duration::from_secs(1)
                )
                .unwrap_err()
                .kind(),
            error.kind()
        );

        let independent = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        assert!(independent
            .check_at(
                &expected,
                start + Duration::from_secs(1),
                start_wall + Duration::from_secs(1)
            )
            .is_ok());
    }

    #[test]
    fn concurrent_terminal_failures_preserve_one_shared_error() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let start = Instant::now();
        let start_wall = wall(90_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let first = context.clone();
        let second = context.clone();
        let expected = repository("https://github.com/owner/repo");
        let barrier = Arc::new(Barrier::new(3));
        let barrier1 = barrier.clone();
        let barrier2 = barrier.clone();
        let first_expected = expected.clone();
        let second_expected = expected.clone();

        let expired = thread::spawn(move || {
            barrier1.wait();
            first
                .check_at(
                    &first_expected,
                    start + MAX_AGE,
                    start_wall + Duration::from_secs(1),
                )
                .unwrap_err()
                .kind()
        });
        let invalid = thread::spawn(move || {
            barrier2.wait();
            second
                .check_at(
                    &second_expected,
                    start.checked_sub(Duration::from_nanos(1)).unwrap(),
                    start_wall + Duration::from_secs(1),
                )
                .unwrap_err()
                .kind()
        });
        barrier.wait();
        let first_kind = expired.join().unwrap();
        let second_kind = invalid.join().unwrap();
        assert_eq!(first_kind, second_kind);
        assert!(matches!(
            first_kind,
            io::ErrorKind::TimedOut | io::ErrorKind::InvalidData
        ));
        assert_eq!(
            context
                .check_at(
                    &expected,
                    start + Duration::from_secs(1),
                    start_wall + Duration::from_secs(1)
                )
                .unwrap_err()
                .kind(),
            first_kind
        );
    }
}
