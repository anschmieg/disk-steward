//! Bounded in-memory name prefix used only to make an explicit page resumable.

#[derive(Clone, Copy)]
struct Limits {
    names: usize,
    raw_bytes: usize,
    encoded_bytes: usize,
    component_bytes: usize,
}

pub(super) struct CheckpointPrefix {
    names: Option<Vec<Vec<u8>>>,
    raw_bytes: usize,
    encoded_bytes: usize,
    blocker: Option<&'static str>,
    limits: Limits,
}

impl Default for CheckpointPrefix {
    fn default() -> Self {
        Self::new(Limits {
            names: super::MAX_CHECKPOINT_NAMES,
            raw_bytes: super::MAX_CHECKPOINT_RAW_BYTES,
            encoded_bytes: super::MAX_CHECKPOINT_ENCODED_BYTES,
            component_bytes: super::MAX_CHECKPOINT_NAME_BYTES,
        })
    }
}

impl CheckpointPrefix {
    #[cfg(test)]
    pub(super) fn unavailable_for_tests() -> Self {
        let mut prefix = Self::default();
        prefix.disable("test_limit");
        prefix
    }

    fn new(limits: Limits) -> Self {
        Self {
            names: Some(Vec::new()),
            raw_bytes: 0,
            encoded_bytes: 2, // outer JSON array brackets
            blocker: None,
            limits,
        }
    }

    pub(super) fn from_names(names: Vec<Vec<u8>>) -> Result<Self, &'static str> {
        let mut prefix = Self::default();
        for name in names {
            if !prefix.push(&name) {
                return Err("Folder checkpoint replay prefix exceeds its bounded budget.");
            }
        }
        Ok(prefix)
    }

    /// Returns false permanently once the checkpoint prefix cannot be saved safely.
    pub(super) fn push(&mut self, name: &[u8]) -> bool {
        let Some(existing_names) = self.names.as_ref() else {
            return false;
        };
        if !valid_component(name, self.limits.component_bytes) {
            self.disable("invalid_component");
            return false;
        }
        if existing_names.len() >= self.limits.names {
            self.disable("name_count_limit");
            return false;
        }
        let Some(raw_bytes) = self.raw_bytes.checked_add(name.len()) else {
            self.disable("raw_byte_limit");
            return false;
        };
        if raw_bytes > self.limits.raw_bytes {
            self.disable("raw_byte_limit");
            return false;
        }
        let Some(entry_bytes) = json_byte_array_size(name) else {
            self.disable("encoded_byte_limit");
            return false;
        };
        let separator = if existing_names.is_empty() { 0 } else { 1 };
        let Some(encoded_bytes) = self
            .encoded_bytes
            .checked_add(separator)
            .and_then(|bytes| bytes.checked_add(entry_bytes))
        else {
            self.disable("encoded_byte_limit");
            return false;
        };
        if encoded_bytes > self.limits.encoded_bytes {
            self.disable("encoded_byte_limit");
            return false;
        }

        let mut owned_name = Vec::new();
        if owned_name.try_reserve_exact(name.len()).is_err() {
            self.disable("allocation_limit");
            return false;
        }
        owned_name.extend_from_slice(name);
        let Some(existing_names) = self.names.as_mut() else {
            return false;
        };
        if existing_names.try_reserve(1).is_err() {
            self.disable("allocation_limit");
            return false;
        }
        existing_names.push(owned_name);
        self.raw_bytes = raw_bytes;
        self.encoded_bytes = encoded_bytes;
        true
    }

    pub(super) fn names(&self) -> Option<&[Vec<u8>]> {
        self.names.as_deref()
    }

    pub(super) fn is_available(&self) -> bool {
        self.names.is_some()
    }

    pub(super) fn blocker(&self) -> Option<&'static str> {
        self.blocker
    }

    fn disable(&mut self, reason: &'static str) {
        self.names = None;
        self.blocker = Some(reason);
    }
}

fn valid_component(name: &[u8], max_bytes: usize) -> bool {
    !name.is_empty()
        && name.len() <= max_bytes
        && !name.contains(&0)
        && !name.contains(&b'/')
        && name != b"."
        && name != b".."
}

fn json_byte_array_size(bytes: &[u8]) -> Option<usize> {
    let digits = bytes.iter().try_fold(0usize, |total, byte| {
        total.checked_add(if *byte < 10 {
            1
        } else if *byte < 100 {
            2
        } else {
            3
        })
    })?;
    // `Vec<u8>` serializes as a JSON array: [12,255].
    2usize
        .checked_add(digits)?
        .checked_add(bytes.len().saturating_sub(1))
}

#[cfg(test)]
impl CheckpointPrefix {
    fn with_limits(
        names: usize,
        raw_bytes: usize,
        encoded_bytes: usize,
        component_bytes: usize,
    ) -> Self {
        Self::new(Limits {
            names,
            raw_bytes,
            encoded_bytes,
            component_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_raw_name_count_and_json_encoded_boundaries_are_accepted() {
        let mut names = CheckpointPrefix::with_limits(2, 2, 11, 1);
        assert!(names.push(b"a"));
        assert!(names.push(b"b"));
        assert_eq!(names.names().unwrap(), &[b"a".to_vec(), b"b".to_vec()]);

        let mut encoded = CheckpointPrefix::with_limits(4, 10, 11, 2);
        // [[97],[98]] is exactly eleven UTF-8 bytes.
        assert!(encoded.push(b"a"));
        assert!(encoded.push(b"b"));
        assert_eq!(encoded.encoded_bytes, 11);
    }

    #[test]
    fn exceeding_any_bound_or_invalid_component_disables_tracking_permanently() {
        for (raw_limit, encoded_limit, component_limit, name, reason) in [
            (1, 100, 10, &b"ab"[..], "raw_byte_limit"),
            (10, 5, 10, &b"a"[..], "encoded_byte_limit"),
            (10, 100, 1, &b"ab"[..], "invalid_component"),
            (10, 100, 10, &b""[..], "invalid_component"),
            (10, 100, 10, &b"a/b"[..], "invalid_component"),
            (10, 100, 10, &b"a\0b"[..], "invalid_component"),
            (10, 100, 10, &b"."[..], "invalid_component"),
            (10, 100, 10, &b".."[..], "invalid_component"),
        ] {
            let mut prefix =
                CheckpointPrefix::with_limits(10, raw_limit, encoded_limit, component_limit);
            assert!(!prefix.push(name));
            assert!(!prefix.is_available());
            assert!(prefix.names().is_none());
            assert!(!prefix.push(b"later"));
            assert_eq!(prefix.blocker(), Some(reason));
        }

        let mut names = CheckpointPrefix::with_limits(1, 10, 100, 10);
        assert!(names.push(b"a"));
        assert!(!names.push(b"b"));
        assert_eq!(names.blocker(), Some("name_count_limit"));
        assert!(!names.push(b"c"));
        assert_eq!(names.names(), None);
    }

    #[test]
    fn preserves_non_utf8_components_and_json_byte_size_is_exact() {
        let mut prefix = CheckpointPrefix::with_limits(2, 3, 32, 255);
        assert!(prefix.push(b"\xff"));
        assert!(prefix.push(b"\x01\xfe"));
        let encoded = serde_json::to_vec(&prefix.names().unwrap()).unwrap();
        assert_eq!(prefix.encoded_bytes, encoded.len());

        let mut max_name = CheckpointPrefix::with_limits(1, 255, 2048, 255);
        assert!(max_name.push(&vec![0xff; 255]));
        let mut oversized_name = CheckpointPrefix::with_limits(1, 256, 4096, 255);
        assert!(!oversized_name.push(&vec![0xff; 256]));
    }
}
