use disk_steward_generated_evidence::{classify_pytest_record, RecordKind};

#[test]
fn recognizes_actual_pytest_920_output_without_provenance_claims() {
    let nodeids =
        include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/nodeids.json");
    let failures =
        include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/lastfailed.json");
    let ids = classify_pytest_record(RecordKind::NodeIds, nodeids).unwrap();
    let failed = classify_pytest_record(RecordKind::LastFailed, failures).unwrap();
    assert_eq!(ids.record_count, 2);
    assert_eq!(failed.record_count, 1);
    assert_eq!(ids.reference_version, "9.2.0");
    assert!(!format!("{ids:?}{failed:?}").contains("test_generated"));
}

#[test]
fn equal_formats_cannot_identify_which_tool_version_produced_them() {
    // Independently generated outputs happen to be identical. The returned
    // reference label is a tested format reference, never a detected version.
    let old = include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.0.2/nodeids.json");
    let current =
        include_bytes!("../../../probes/generated_cache/fixtures/pytest-9.2.0/nodeids.json");
    assert_eq!(old, current);
    assert_eq!(
        classify_pytest_record(RecordKind::NodeIds, old).unwrap(),
        classify_pytest_record(RecordKind::NodeIds, current).unwrap()
    );
}
