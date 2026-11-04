//! Bounded streaming primitives for explicit native duplicate-content checks.
//! No file contents are retained after each call.

use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_READ_BYTES: u64 = 192 * 1024 * 1024;
pub(super) const MAX_FILES: usize = 32;
pub(super) const MAX_DURATION: Duration = Duration::from_secs(5);
const BUFFER_BYTES: usize = 8 * 1024;

pub(super) struct ReadBudget<'a> {
    cancel: &'a AtomicBool,
    started: Instant,
    duration: Duration,
    bytes_read: u64,
    read_limit: u64,
}

impl<'a> ReadBudget<'a> {
    pub(super) fn new(cancel: &'a AtomicBool, started: Instant, duration: Duration) -> Self {
        Self {
            cancel,
            started,
            duration: duration.min(MAX_DURATION),
            bytes_read: 0,
            read_limit: MAX_READ_BYTES,
        }
    }

    pub(super) fn check(&self) -> Result<(), &'static str> {
        if self.cancel.load(Ordering::Acquire) {
            return Err("Duplicate-content inspection was canceled.");
        }
        if self.started.elapsed() >= self.duration {
            return Err("Duplicate-content inspection exceeded its cooperative time limit.");
        }
        Ok(())
    }

    pub(super) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    fn record_read(&mut self, bytes: usize) -> Result<(), &'static str> {
        let bytes = u64::try_from(bytes)
            .map_err(|_| "Duplicate-content read exceeded its supported range.")?;
        self.bytes_read = self
            .bytes_read
            .checked_add(bytes)
            .ok_or("Duplicate-content read exceeded its supported range.")?;
        if self.bytes_read > self.read_limit {
            return Err("Duplicate-content inspection exceeded its read limit.");
        }
        Ok(())
    }
}

pub(super) fn fingerprint<R: Read>(
    reader: &mut R,
    expected_bytes: u64,
    budget: &mut ReadBudget<'_>,
) -> Result<[u8; 32], &'static str> {
    budget.check()?;
    if expected_bytes > MAX_FILE_BYTES {
        return Err("Selected file exceeds the duplicate-content size limit.");
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; BUFFER_BYTES];
    let mut consumed = 0u64;
    while consumed < expected_bytes {
        budget.check()?;
        let remaining = expected_bytes - consumed;
        let requested = usize::try_from(remaining.min(BUFFER_BYTES as u64))
            .map_err(|_| "Duplicate-content read exceeded its supported range.")?;
        let count = read_retry(reader, &mut buffer[..requested], budget)?;
        if count == 0 {
            return Err("Selected file ended before its observed size.");
        }
        hasher.update(&buffer[..count]);
        consumed = consumed
            .checked_add(count as u64)
            .ok_or("Duplicate-content read exceeded its supported range.")?;
    }

    ensure_eof(reader, budget)?;
    budget.check()?;
    let digest = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    budget.check()?;
    Ok(output)
}

pub(super) fn equal<R: Read, S: Read>(
    left: &mut R,
    right: &mut S,
    expected_bytes: u64,
    budget: &mut ReadBudget<'_>,
) -> Result<bool, &'static str> {
    budget.check()?;
    if expected_bytes > MAX_FILE_BYTES {
        return Err("Selected file exceeds the duplicate-content size limit.");
    }

    let mut left_buffer = [0u8; BUFFER_BYTES];
    let mut right_buffer = [0u8; BUFFER_BYTES];
    let mut compared = 0u64;
    let mut matches = true;
    while compared < expected_bytes {
        budget.check()?;
        let count = usize::try_from((expected_bytes - compared).min(BUFFER_BYTES as u64))
            .map_err(|_| "Duplicate-content read exceeded its supported range.")?;
        fill_exact(left, &mut left_buffer[..count], budget)?;
        fill_exact(right, &mut right_buffer[..count], budget)?;
        if left_buffer[..count] != right_buffer[..count] {
            matches = false;
        }
        compared = compared
            .checked_add(count as u64)
            .ok_or("Duplicate-content read exceeded its supported range.")?;
        budget.check()?;
    }

    let left_extra = probe_eof(left, budget)?;
    let right_extra = probe_eof(right, budget)?;
    if left_extra || right_extra {
        return Err("Selected file length changed during duplicate-content inspection.");
    }
    budget.check()?;
    Ok(matches)
}

fn fill_exact<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    budget: &mut ReadBudget<'_>,
) -> Result<(), &'static str> {
    let mut filled = 0usize;
    while filled < buffer.len() {
        let count = read_retry(reader, &mut buffer[filled..], budget)?;
        if count == 0 {
            return Err("Selected file ended before its observed size.");
        }
        filled = filled
            .checked_add(count)
            .ok_or("Duplicate-content read exceeded its supported range.")?;
    }
    Ok(())
}

fn ensure_eof<R: Read>(reader: &mut R, budget: &mut ReadBudget<'_>) -> Result<(), &'static str> {
    if probe_eof(reader, budget)? {
        Err("Selected file length changed during duplicate-content inspection.")
    } else {
        Ok(())
    }
}

fn probe_eof<R: Read>(reader: &mut R, budget: &mut ReadBudget<'_>) -> Result<bool, &'static str> {
    let mut extra = [0u8; 1];
    Ok(read_retry(reader, &mut extra, budget)? != 0)
}

fn read_retry<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    budget: &mut ReadBudget<'_>,
) -> Result<usize, &'static str> {
    loop {
        budget.check()?;
        let allowance = budget.read_limit.saturating_sub(budget.bytes_read);
        if allowance == 0 {
            return Err("Duplicate-content inspection exceeded its read limit.");
        }
        let request = buffer
            .len()
            .min(usize::try_from(allowance).unwrap_or(usize::MAX));
        let result = reader.read(&mut buffer[..request]);
        budget.check()?;
        match result {
            Ok(count) => {
                let recorded = budget.record_read(count);
                budget.check()?;
                recorded?;
                return Ok(count);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                budget.check()?;
            }
            Err(_) => return Err("Could not read selected file contents."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct PartialReader<'a> {
        bytes: &'a [u8],
        position: usize,
        max_chunk: usize,
    }

    impl<'a> PartialReader<'a> {
        fn new(bytes: &'a [u8], max_chunk: usize) -> Self {
            Self {
                bytes,
                position: 0,
                max_chunk,
            }
        }
    }

    impl Read for PartialReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let available = self.bytes.len().saturating_sub(self.position);
            let count = available.min(buffer.len()).min(self.max_chunk);
            buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }
    }

    struct ErrorReader;
    impl Read for ErrorReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("private path and provider detail"))
        }
    }

    fn budget(cancel: &AtomicBool) -> ReadBudget<'_> {
        ReadBudget::new(cancel, Instant::now(), MAX_DURATION)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn fingerprint_matches_known_sha256_and_handles_partial_reads() {
        let cancel = AtomicBool::new(false);
        let mut budget = budget(&cancel);
        let mut reader = PartialReader::new(b"abc", 1);
        let digest = fingerprint(&mut reader, 3, &mut budget).unwrap();
        assert_eq!(
            hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(budget.bytes_read(), 3);
    }

    #[test]
    fn equality_handles_different_partial_chunk_boundaries_and_reads_full_inputs() {
        let cancel = AtomicBool::new(false);
        let mut budget = budget(&cancel);
        let mut left = PartialReader::new(b"same content", 2);
        let mut right = PartialReader::new(b"same content", 5);
        assert!(equal(&mut left, &mut right, 12, &mut budget).unwrap());
        assert_eq!(budget.bytes_read(), 24);

        let mut unequal_budget = self::budget(&cancel);
        let mut left = PartialReader::new(b"same content", 3);
        let mut right = PartialReader::new(b"same Content", 4);
        assert!(!equal(&mut left, &mut right, 12, &mut unequal_budget).unwrap());
        assert_eq!(unequal_budget.bytes_read(), 24);
    }

    #[test]
    fn exact_length_checks_cover_empty_truncated_and_extra_inputs() {
        let cancel = AtomicBool::new(false);
        let mut empty_budget = budget(&cancel);
        let mut empty = Cursor::new(Vec::<u8>::new());
        assert_eq!(
            hex(&fingerprint(&mut empty, 0, &mut empty_budget).unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(equal(
            &mut Cursor::new(Vec::<u8>::new()),
            &mut Cursor::new(Vec::<u8>::new()),
            0,
            &mut empty_budget,
        )
        .unwrap());

        let mut truncated_budget = budget(&cancel);
        assert!(fingerprint(&mut Cursor::new(b"abc"), 4, &mut truncated_budget).is_err());
        assert_eq!(truncated_budget.bytes_read(), 3);

        let mut extra_budget = budget(&cancel);
        assert!(fingerprint(&mut Cursor::new(b"abcd"), 3, &mut extra_budget).is_err());
        assert_eq!(extra_budget.bytes_read(), 4);
    }

    #[test]
    fn io_errors_are_sanitized_and_cancellation_or_deadline_stop_before_read() {
        let cancel = AtomicBool::new(false);
        let mut io_budget = budget(&cancel);
        assert_eq!(
            fingerprint(&mut ErrorReader, 1, &mut io_budget).unwrap_err(),
            "Could not read selected file contents."
        );

        let canceled = AtomicBool::new(true);
        let mut canceled_budget = budget(&canceled);
        assert!(fingerprint(&mut Cursor::new(b"x"), 1, &mut canceled_budget).is_err());
        assert_eq!(canceled_budget.bytes_read(), 0);

        let live = AtomicBool::new(false);
        let mut expired_budget = ReadBudget::new(&live, Instant::now(), Duration::ZERO);
        assert!(fingerprint(&mut Cursor::new(b"x"), 1, &mut expired_budget).is_err());
        assert_eq!(expired_budget.bytes_read(), 0);
    }

    #[test]
    fn reduced_read_limit_fails_without_large_fixture_allocation() {
        let cancel = AtomicBool::new(false);
        let mut budget = budget(&cancel);
        budget.read_limit = 2;
        let mut reader = Cursor::new(b"abc");
        assert!(fingerprint(&mut reader, 3, &mut budget).is_err());
        assert_eq!(budget.bytes_read(), 2);
        assert_eq!(reader.position(), 2);
    }
}
