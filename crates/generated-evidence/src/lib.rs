//! Strict, bounded recognition of selected pytest cache record formats.
//!
//! This crate accepts only caller-provided bytes. It performs no file access,
//! path resolution, process execution, or cleanup decision. Recognizing this
//! structure does not prove the installed pytest version, which version produced
//! it, ownership, freshness, idleness, regenerability, or cleanup eligibility.

use std::{collections::BTreeSet, fmt};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

pub const DETECTOR_VERSION: &str = "pytest-cache-format-v1";
pub const REFERENCE_PYTEST_VERSION: &str = "9.2.0";
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_RECORDS: usize = 512;
pub const MAX_NODE_ID_BYTES: usize = 1_024;

/// The one supported top-level pytest cache record shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    /// A JSON array of unique pytest node IDs.
    NodeIds,
    /// A JSON object mapping unique pytest node IDs to the literal `true`.
    LastFailed,
}

/// Bounded structural evidence. It deliberately contains no node IDs or input bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordEvidence {
    pub kind: RecordKind,
    pub record_count: u32,
    pub detector_version: &'static str,
    pub reference_version: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    InputTooLarge,
    InvalidUtf8,
    ExcessiveNesting,
    InvalidFormat,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InputTooLarge => "input exceeds the byte limit",
            Self::InvalidUtf8 => "input is not UTF-8",
            Self::ExcessiveNesting => "input exceeds the structural nesting limit",
            Self::InvalidFormat => "input is not a supported pytest cache record",
        })
    }
}

impl std::error::Error for RecordError {}

/// Recognizes one strict pytest cache record shape from bounded in-memory bytes.
///
/// Node IDs are validated and discarded while parsing. The result reports only
/// a count and fixed format/version labels; it is not proof of installed tool
/// version, provenance, freshness, ownership, idleness, regenerability, or
/// cleanup eligibility.
pub fn classify_pytest_record(
    kind: RecordKind,
    bytes: &[u8],
) -> Result<RecordEvidence, RecordError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(RecordError::InputTooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| RecordError::InvalidUtf8)?;
    check_json_nesting(text.as_bytes())?;

    let count = match kind {
        RecordKind::NodeIds => {
            let records: NodeIdArray =
                serde_json::from_str(text).map_err(|_| RecordError::InvalidFormat)?;
            records.0.len()
        }
        RecordKind::LastFailed => {
            let records: LastFailedMap =
                serde_json::from_str(text).map_err(|_| RecordError::InvalidFormat)?;
            records.0.len()
        }
    };
    let record_count = u32::try_from(count).map_err(|_| RecordError::InvalidFormat)?;
    Ok(RecordEvidence {
        kind,
        record_count,
        detector_version: DETECTOR_VERSION,
        reference_version: REFERENCE_PYTEST_VERSION,
    })
}

/// JSON nesting for these formats is exactly one container level. This lexical
/// preflight ignores delimiters inside quoted strings; serde performs full JSON
/// grammar and escape validation after the byte and depth bounds are enforced.
fn check_json_nesting(bytes: &[u8]) -> Result<(), RecordError> {
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0_u8;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth = depth.checked_add(1).ok_or(RecordError::ExcessiveNesting)?;
                if depth > 1 {
                    return Err(RecordError::ExcessiveNesting);
                }
            }
            b']' | b'}' => {
                if depth == 0 {
                    return Err(RecordError::InvalidFormat);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    Ok(())
}

struct NodeIdArray(Vec<String>);

impl<'de> Deserialize<'de> for NodeIdArray {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(NodeIdArrayVisitor)
    }
}

struct NodeIdArrayVisitor;

impl<'de> Visitor<'de> for NodeIdArrayVisitor {
    type Value = NodeIdArray;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of unique pytest node IDs")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut records = Vec::new();
        let mut unique = BTreeSet::new();
        while let Some(node_id) = seq.next_element::<String>()? {
            if records.len() == MAX_RECORDS
                || !valid_node_id(&node_id)
                || !unique.insert(node_id.clone())
            {
                return Err(de::Error::custom("invalid or excessive pytest node ID"));
            }
            records.push(node_id);
        }
        Ok(NodeIdArray(records))
    }
}

struct LastFailedMap(Vec<String>);

impl<'de> Deserialize<'de> for LastFailedMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(LastFailedVisitor)
    }
}

struct LastFailedVisitor;

impl<'de> Visitor<'de> for LastFailedVisitor {
    type Value = LastFailedMap;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object mapping unique pytest node IDs to true")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut records = Vec::new();
        let mut unique = BTreeSet::new();
        while let Some(node_id) = map.next_key::<String>()? {
            if records.len() == MAX_RECORDS
                || !valid_node_id(&node_id)
                || !unique.insert(node_id.clone())
            {
                return Err(de::Error::custom(
                    "invalid, duplicate, or excessive pytest node ID",
                ));
            }
            let value = map.next_value::<bool>()?;
            if !value {
                return Err(de::Error::custom("lastfailed values must be true"));
            }
            records.push(node_id);
        }
        Ok(LastFailedMap(records))
    }
}

fn valid_node_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_NODE_ID_BYTES || value.chars().any(char::is_control) {
        return false;
    }
    let Some((path, test_name)) = value.split_once("::") else {
        return false;
    };
    if path.is_empty()
        || test_name.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.contains("://")
        || !path.ends_with(".py")
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(kind: RecordKind, input: &str) -> Result<RecordEvidence, RecordError> {
        classify_pytest_record(kind, input.as_bytes())
    }

    #[test]
    fn accepts_empty_and_unicode_records_without_returning_names() {
        let empty = classify(RecordKind::NodeIds, "[]").unwrap();
        assert_eq!(empty.record_count, 0);
        assert_eq!(empty.kind, RecordKind::NodeIds);
        assert_eq!(empty.detector_version, "pytest-cache-format-v1");
        assert_eq!(empty.reference_version, "9.2.0");
        assert_eq!(
            classify(RecordKind::LastFailed, "{}").unwrap().record_count,
            0
        );
        let unicode = classify(RecordKind::NodeIds, r#"["tests/ümlaut.py::test_é"]"#).unwrap();
        assert_eq!(unicode.record_count, 1);
        assert!(!format!("{unicode:?}").contains("test_é"));
    }

    #[test]
    fn enforces_unique_node_ids_and_literal_true_map_values() {
        assert_eq!(
            classify(
                RecordKind::NodeIds,
                r#"["tests/a.py::test_x","tests/a.py::test_x"]"#
            ),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(
                RecordKind::LastFailed,
                r#"{"tests/a.py::test_x":true,"tests/a.py::test_x":true}"#
            ),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::LastFailed, r#"{"tests/a.py::test_x":false}"#),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::LastFailed, r#"{"tests/a.py::test_x":1}"#),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::LastFailed, r#"{"tests/a.py::test_x":[]}"#),
            Err(RecordError::ExcessiveNesting)
        );
    }

    #[test]
    fn rejects_traversal_absolute_drive_uri_and_non_python_node_ids() {
        for id in [
            "../tests/a.py::test_x",
            "tests/../a.py::test_x",
            "tests/./a.py::test_x",
            "/tests/a.py::test_x",
            "C:/tests/a.py::test_x",
            "file://host/tests/a.py::test_x",
            "tests\\a.py::test_x",
            "tests/a.txt::test_x",
            "tests/a.py",
            "tests//a.py::test_x",
        ] {
            let input = serde_json::to_string(&vec![id]).unwrap();
            assert_eq!(
                classify_pytest_record(RecordKind::NodeIds, input.as_bytes()),
                Err(RecordError::InvalidFormat),
                "{id}"
            );
        }
    }

    #[test]
    fn rejects_input_record_node_length_utf8_and_json_shape_bounds() {
        assert_eq!(
            classify_pytest_record(RecordKind::NodeIds, &vec![b' '; MAX_INPUT_BYTES + 1]),
            Err(RecordError::InputTooLarge)
        );
        assert_eq!(
            classify_pytest_record(RecordKind::NodeIds, &[0xff]),
            Err(RecordError::InvalidUtf8)
        );
        let at_limit = serde_json::to_vec(
            &(0..MAX_RECORDS)
                .map(|i| format!("tests/t.py::case_{i}"))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            classify_pytest_record(RecordKind::NodeIds, &at_limit)
                .unwrap()
                .record_count,
            MAX_RECORDS as u32
        );
        let over_limit = serde_json::to_vec(
            &(0..=MAX_RECORDS)
                .map(|i| format!("tests/t.py::case_{i}"))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            classify_pytest_record(RecordKind::NodeIds, &over_limit),
            Err(RecordError::InvalidFormat)
        );
        let at_byte_limit = format!("[]{}", " ".repeat(MAX_INPUT_BYTES - 2));
        assert_eq!(
            classify_pytest_record(RecordKind::NodeIds, at_byte_limit.as_bytes())
                .unwrap()
                .record_count,
            0
        );
        let long_id = format!("tests/a.py::{}", "x".repeat(MAX_NODE_ID_BYTES));
        assert_eq!(
            classify_pytest_record(
                RecordKind::NodeIds,
                serde_json::to_string(&vec![long_id]).unwrap().as_bytes()
            ),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::NodeIds, "[[\"tests/a.py::test_x\"]]"),
            Err(RecordError::ExcessiveNesting)
        );
        assert_eq!(
            classify(RecordKind::NodeIds, "[] []"),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::NodeIds, "{}"),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::LastFailed, "[]"),
            Err(RecordError::InvalidFormat)
        );
        assert_eq!(
            classify(RecordKind::NodeIds, r#"["tests/a.py::test\u0000x"]"#),
            Err(RecordError::InvalidFormat)
        );
    }
}
