use crate::RepositoryUrl;
use std::io;
use std::time::{Duration, Instant, SystemTime};

const MAX_AGE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttemptContext {
    repository: RepositoryUrl,
    started_at: Instant,
    started_wall: SystemTime,
}

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

        let age = now.checked_duration_since(self.started_at).ok_or_else(|| {
            safe_error(
                io::ErrorKind::InvalidData,
                "verification clock context invalid",
            )
        })?;
        let wall_age = wall.duration_since(self.started_wall).map_err(|_| {
            safe_error(
                io::ErrorKind::InvalidData,
                "verification clock context invalid",
            )
        })?;
        if age >= MAX_AGE || wall_age >= MAX_AGE {
            return Err(safe_error(
                io::ErrorKind::TimedOut,
                "verification context expired",
            ));
        }
        Ok(())
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
        let start = Instant::now();
        let start_wall = wall(20_000);
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            start_wall,
        );
        let expected = repository("https://github.com/owner/repo");

        for (now, wall_now) in [
            (start + MAX_AGE, start_wall + Duration::from_secs(1)),
            (start + Duration::from_secs(1), start_wall + MAX_AGE),
        ] {
            let error = context.check_at(&expected, now, wall_now).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string(), "verification context expired");
        }
    }

    #[test]
    fn an_earlier_clock_instant_is_rejected() {
        let start = Instant::now();
        let context = AttemptContext::new(
            repository("https://github.com/owner/repo"),
            start,
            wall(30_000),
        );
        let error = context
            .check_at(
                context.repository(),
                start.checked_sub(Duration::from_nanos(1)).unwrap(),
                wall(30_001),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "verification clock context invalid");

        let error = context
            .check_at(context.repository(), start, wall(29_999))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "verification clock context invalid");
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
}
