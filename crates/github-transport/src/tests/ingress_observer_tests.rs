use super::pack_tests::{pack_body, pack_fixture};
use crate::TransportIngressObserver;

fn completed() -> (TransportIngressObserver, crate::PackTransferReport) {
    let (transport, server) = pack_fixture(pack_body(b"PACKfixture"));
    let observer = transport.ingress_observer();
    assert_eq!(observer.observed_bytes().unwrap(), 0);
    let mut sink = Vec::new();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(report.is_complete_transfer());
    (observer, report)
}

#[test]
fn observer_survives_consumption_and_clones_share_cumulative_counter() {
    let (transport, server) = pack_fixture(pack_body(b"PACKfixture"));
    let observer = transport.ingress_observer();
    let clone = observer.clone();
    let signal = transport.cancellation_handle();
    let mut sink = Vec::new();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(report.is_complete_transfer());
    let total = report.finalization().observed_ingress_bytes();
    assert!(total > report.bytes_written());
    assert_eq!(observer.observed_bytes().unwrap(), total);
    assert!(observer.matches_transfer(&report));
    signal.cancel();
    // Cancellation must not hide bytes already observed or change historical identity.
    assert_eq!(observer.observed_bytes().unwrap(), total);
    let report_clone = report.clone();
    std::thread::spawn(move || {
        assert_eq!(clone.observed_bytes().unwrap(), total);
        assert!(clone.matches_transfer(&report_clone));
    })
    .join()
    .unwrap();
}

#[test]
fn equal_repository_and_payload_do_not_merge_attempt_identity() {
    let (first, first_report) = completed();
    let (second, second_report) = completed();
    assert_eq!(first_report.repository(), second_report.repository());
    assert_eq!(
        first_report.accepted_pack_sha256(),
        second_report.accepted_pack_sha256()
    );
    assert!(first.matches_transfer(&first_report));
    assert!(second.matches_transfer(&second_report));
    assert!(!first.matches_transfer(&second_report));
    assert!(!second.matches_transfer(&first_report));
}

#[test]
fn poisoned_counter_returns_sanitized_error_without_panicking() {
    let (transport, server) = pack_fixture(pack_body(b"PACKfixture"));
    let counter = transport.budget.used.clone();
    let observer = transport.ingress_observer();
    let report = transport.download_advertised_pack(&mut Vec::new()).unwrap();
    server.join().unwrap();
    assert!(report.is_complete_transfer());
    let poisoned = std::thread::spawn(move || {
        let _lock = counter.lock().unwrap();
        panic!("synthetic counter poison");
    })
    .join();
    assert!(poisoned.is_err());
    let error = observer.observed_bytes().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(error.to_string(), "ingress observation unavailable");
    assert!(observer.matches_transfer(&report));
}
