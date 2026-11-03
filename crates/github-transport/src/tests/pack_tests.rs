use super::*;

pub(super) fn pack_fixture(body: Vec<u8>) -> (GitHubTransport, thread::JoinHandle<()>) {
    pack_fixture_with_framing(body, false)
}

fn pack_fixture_with_framing(
    body: Vec<u8>,
    truncated_http: bool,
) -> (GitHubTransport, thread::JoinHandle<()>) {
    pack_fixture_with_refs(
        body,
        truncated_http,
        b"0123456789abcdef0123456789abcdef01234567 HEAD\n".to_vec(),
    )
}

pub(super) fn pack_fixture_with_refs(
    body: Vec<u8>,
    truncated_http: bool,
    advertised: Vec<u8>,
) -> (GitHubTransport, thread::JoinHandle<()>) {
    let tls = fixture_tls();
    let (listener, address) = listener();
    let config = Arc::clone(&tls.server);
    let server = thread::spawn(move || {
        let caps = ref_caps(&[b"ls-refs\n", b"fetch\n", b"object-format=sha1\n"]);
        let mut refs = Vec::new();
        for line in advertised.split_inclusive(|b| *b == b'\n') {
            refs.extend(ref_packet(line));
        }
        refs.extend_from_slice(b"0000");
        for (index, (method, ct, response)) in [
            ("GET", "application/x-git-upload-pack-advertisement", &caps),
            ("POST", "application/x-git-upload-pack-result", &refs),
            ("POST", "application/x-git-upload-pack-result", &body),
        ]
        .into_iter()
        .enumerate()
        {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, Arc::clone(&config)).unwrap();
            let (actual, target, request, _) = read_request(&mut stream).unwrap();
            assert_eq!(actual, method);
            if method == "GET" {
                assert_eq!(
                    target,
                    "/example/repo.git/info/refs?service=git-upload-pack"
                );
            } else {
                assert_eq!(target, "/example/repo.git/git-upload-pack");
                assert!(request.ends_with(b"0000"));
            }
            // A sink failure may close the connection during finalization.
            if truncated_http && index == 2 {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n",
                    response.len() + 1
                )
                .unwrap();
                stream.write_all(response).unwrap();
                stream.flush().unwrap();
            } else {
                let _ = write_response(&mut stream, ct, response);
            }
        }
    });
    (
        transport_at(address, tls.trusted, AttemptLimits::default()),
        server,
    )
}

pub(super) fn pack_body(payload: &[u8]) -> Vec<u8> {
    let mut body = ref_packet(b"packfile\n");
    let mut sideband = vec![1];
    sideband.extend_from_slice(payload);
    body.extend(ref_packet(&sideband));
    body.extend_from_slice(b"0000");
    body
}

#[test]
fn pack_transfer_completion_is_not_an_object_integrity_claim() {
    let (transport, server) = pack_fixture(pack_body(b"PACKopaque fixture bytes"));
    let mut sink = Vec::new();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(report.is_complete_transfer());
    let claims = report.advertised_refs().unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].name(), b"HEAD");
    assert_eq!(
        claims[0].oid(),
        &[
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef, 0x01, 0x23, 0x45, 0x67
        ]
    );
    assert_eq!(sink, b"PACKopaque fixture bytes");
    assert_eq!(report.bytes_written(), sink.len() as u64);
    let digest_hex: String = report
        .accepted_pack_sha256()
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest_hex,
        "ed2cf8c3289ae21bfcc63b511e4e7cb82b50e6b0cd67c43bf0f6903bb1a07433"
    );
    assert_eq!(report.finalization().framing().completed(), 3);
}

#[test]
fn pack_transfer_rejects_protocol_failures_without_complete_report() {
    let mut bad_channel = ref_packet(b"packfile\n");
    bad_channel.extend(ref_packet(b"\x04unsupported"));
    bad_channel.extend_from_slice(b"0000");
    let mut fatal = ref_packet(b"packfile\n");
    fatal.extend(ref_packet(b"\x03fixture diagnostic must not escape"));
    fatal.extend_from_slice(b"0000");
    let mut truncated = pack_body(b"partial");
    truncated.truncate(truncated.len() - 4);
    for body in [bad_channel, fatal, truncated, ref_packet(b"shallow-info\n")] {
        let (transport, server) = pack_fixture(body);
        let mut sink = Vec::new();
        let report = transport.download_advertised_pack(&mut sink).unwrap();
        server.join().unwrap();
        assert!(!report.is_complete_transfer());
        assert!(report.advertised_refs().is_none());
        assert!(report.accepted_pack_sha256().is_none());
        assert!(report.operation_error_kind().is_some());
        assert_eq!(report.bytes_written(), sink.len() as u64);
    }
}

struct PartialFailure {
    bytes: Vec<u8>,
}
impl io::Write for PartialFailure {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.bytes.is_empty() {
            return Err(io::Error::other("private sink diagnostic"));
        }
        let count = data.len().min(3);
        self.bytes.extend_from_slice(&data[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn pack_transfer_reports_actual_partial_sink_bytes_on_failure() {
    let (transport, server) = pack_fixture(pack_body(b"PACKpartial write"));
    let mut sink = PartialFailure { bytes: Vec::new() };
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(!report.is_complete_transfer());
    assert!(report.advertised_refs().is_none());
    assert!(report.accepted_pack_sha256().is_none());
    assert_eq!(report.bytes_written(), 3);
    assert_eq!(sink.bytes, b"PAC");
    assert_eq!(report.operation_error_kind(), Some(io::ErrorKind::Other));
    assert!(!format!("{report:?}").contains("private sink diagnostic"));
}

#[test]
fn oversized_or_empty_tip_set_refuses_fetch_before_sink_io() {
    let mut too_many = Vec::new();
    for i in 1..=65 {
        too_many.extend(ref_packet(
            format!("{i:040x} refs/heads/fixture-{i}\n").as_bytes(),
        ));
    }
    too_many.extend_from_slice(b"0000");
    for response in [b"0000".to_vec(), too_many] {
        let (transport, server) =
            ref_transport_fixture(ref_caps(&[b"ls-refs\n", b"fetch\n"]), Some(response));
        let mut sink = Vec::new();
        let report = transport.download_advertised_pack(&mut sink).unwrap();
        server.join().unwrap();
        assert!(!report.is_complete_transfer());
        assert!(report.advertised_refs().is_none());
        assert!(report.accepted_pack_sha256().is_none());
        assert_eq!(
            report.operation_error_kind(),
            Some(io::ErrorKind::InvalidData)
        );
        assert_eq!(report.bytes_written(), 0);
        assert!(sink.is_empty());
        let framing = report.finalization().framing();
        assert_eq!(
            framing.pending() + framing.completed() + framing.failed() + framing.abandoned(),
            2
        );
        assert!(!report.finalization().is_complete());
    }
}

struct CancelAfterWrite {
    signal: CancellationHandle,
    accepted: usize,
}
impl io::Write for CancelAfterWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.accepted += bytes.len();
        self.signal.cancel();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn sink_cancellation_keeps_accepted_bytes_and_prevents_further_writes() {
    let (transport, server) = pack_fixture(pack_body(b"PACKtail"));
    let mut sink = CancelAfterWrite {
        signal: transport.cancellation_handle(),
        accepted: 0,
    };
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(!report.is_complete_transfer());
    assert!(report.advertised_refs().is_none());
    assert!(report.accepted_pack_sha256().is_none());
    assert_eq!(
        report.operation_error_kind(),
        Some(io::ErrorKind::ConnectionAborted)
    );
    assert_eq!(report.bytes_written(), 4);
    assert_eq!(sink.accepted, 4);
}

#[test]
fn git_flush_cannot_complete_transfer_with_truncated_http_framing() {
    let (transport, server) = pack_fixture_with_framing(pack_body(b"PACKopaque"), true);
    let mut sink = Vec::new();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert_eq!(sink, b"PACKopaque");
    assert_eq!(report.bytes_written(), sink.len() as u64);
    assert!(!report.is_complete_transfer());
    assert!(report.advertised_refs().is_none());
    assert!(report.accepted_pack_sha256().is_none());
    assert!(!report.finalization().is_complete());
    assert!(report.operation_error_kind().is_none());
}

const BLOCKED_PACK_SINK_READY: &[u8] = b"PACK_SINK_SECOND_WRITE_BLOCKED\n";

struct PrefixThenBlock {
    signal: CancellationHandle,
    accepted: usize,
}

impl io::Write for PrefixThenBlock {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.accepted == 0 {
            assert_eq!(bytes, b"PACK");
            self.accepted = 4;
            return Ok(4);
        }

        assert_eq!(self.accepted, 4);
        assert_eq!(bytes, b"fixture tail");
        self.signal.cancel();
        assert!(self.signal.is_cancelled());
        let mut stdout = io::stdout();
        stdout.write_all(BLOCKED_PACK_SINK_READY)?;
        stdout.flush()?;
        loop {
            thread::park();
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "invoked only by the exact blocked pack-sink deadline parent"]
fn blocked_pack_sink_child() {
    let (transport, _server) = pack_fixture(pack_body(b"PACKfixture tail"));
    let mut sink = PrefixThenBlock {
        signal: transport.cancellation_handle(),
        accepted: 0,
    };
    let _report = transport.download_advertised_pack(&mut sink);
    panic!("candidate returned from the deliberately blocked pack sink");
}

#[test]
fn outer_deadline_stops_blocked_pack_sink() {
    super::assert_outer_deadline_stops_child(
        "tests::pack_tests::blocked_pack_sink_child",
        BLOCKED_PACK_SINK_READY,
    );
}

fn advertised_decode_fixture(refs: Vec<u8>, bytes: &[u8]) -> PackTransferReport {
    let (transport, server) = pack_fixture_with_refs(pack_body(bytes), false, refs);
    let mut sink = Vec::new();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert_eq!(sink, bytes);
    assert!(report.is_complete_transfer());
    report
}
fn decode_report(report: &PackTransferReport, bytes: &[u8]) -> io::Result<AdvertisedPackReport> {
    decode_advertised_pack(
        report,
        &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
        bytes,
        16 * 1024 * 1024,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
}
#[test]
fn advertised_pack_closure_binds_all_tip_claims_and_peeled_targets() {
    use super::graph_tests::{commit, hex, oid, pack_bytes, tree};
    use GitObjectKind as K;
    let blob = b"x".to_vec();
    let blob_id = oid(K::Blob, &blob);
    let t = tree("100644", blob_id);
    let tree_id = oid(K::Tree, &t);
    let c = commit(tree_id, &[]);
    let commit_id = oid(K::Commit, &c);
    let tag = format!("object {}\ntype commit\ntag inner\n\n", hex(&commit_id)).into_bytes();
    let tag_id = oid(K::Tag, &tag);
    let outer = format!("object {}\ntype tag\ntag outer\n\n", hex(&tag_id)).into_bytes();
    let outer_id = oid(K::Tag, &outer);
    let bytes = pack_bytes(&[
        (K::Blob, blob),
        (K::Tree, t),
        (K::Commit, c),
        (K::Tag, tag),
        (K::Tag, outer),
    ]);
    let refs=format!("{} HEAD symref-target:refs/heads/main\n{} refs/heads/main\n{} refs/tags/outer peeled:{}\n{} refs/tags/blob\n",hex(&commit_id),hex(&commit_id),hex(&outer_id),hex(&commit_id),hex(&blob_id)).into_bytes();
    let report = advertised_decode_fixture(refs, &bytes);
    let result = decode_report(&report, &bytes).unwrap();
    assert_eq!(result.advertised_ref_count(), 4);
    assert_eq!(result.graph().root_count(), 3);
    assert_eq!(result.graph().reachable_objects(), 5);
    assert_eq!(result.decoded().objects().len(), 5);
    let mut corrupt = bytes.clone();
    corrupt[15] ^= 1;
    assert!(decode_report(&report, &corrupt).is_err());
    assert!(decode_report(&report, &bytes[..bytes.len() - 1]).is_err());
    let cancel = CancellationHandle::new();
    cancel.cancel();
    assert_eq!(
        decode_advertised_pack(
            &report,
            &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
            &bytes,
            4096,
            &cancel,
            Instant::now() + Duration::from_secs(5)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::ConnectionAborted
    );
    assert_eq!(
        decode_advertised_pack(
            &report,
            &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
            &bytes,
            4096,
            &CancellationHandle::new(),
            Instant::now()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    for refs in [
        format!("{} refs/heads/wrong\n", hex(&blob_id)),
        format!("{} HEAD\n", hex(&blob_id)),
        format!("{} refs/tags/missing\n", hex(&[1; 20])),
        format!("{} HEAD symref-target:refs/heads/absent\n", hex(&commit_id)),
        format!(
            "{} HEAD symref-target:refs/tags/blob\n{} refs/tags/blob\n",
            hex(&commit_id),
            hex(&blob_id)
        ),
        format!(
            "{} refs/tags/outer peeled:{}\n",
            hex(&outer_id),
            hex(&tag_id)
        ),
        format!(
            "{} refs/tags/outer peeled:{}\n",
            hex(&outer_id),
            hex(&blob_id)
        ),
        format!(
            "{} refs/tags/blob peeled:{}\n",
            hex(&blob_id),
            hex(&blob_id)
        ),
    ] {
        let report = advertised_decode_fixture(refs.into_bytes(), &bytes);
        assert!(decode_report(&report, &bytes).is_err());
    }
}
#[test]
fn advertised_pack_decode_refuses_incomplete_transfer_and_missing_graph_links() {
    use super::graph_tests::{commit, hex, oid, pack_bytes};
    use GitObjectKind as K;
    let bytes = pack_bytes(&[(K::Blob, b"x".to_vec())]);
    let (transport, server) = pack_fixture_with_framing(pack_body(&bytes), true);
    let mut sink = Vec::new();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(!report.is_complete_transfer());
    assert!(decode_report(&report, &bytes).is_err());
    let c = commit([1; 20], &[]);
    let id = oid(K::Commit, &c);
    let bytes = pack_bytes(&[(K::Commit, c)]);
    let report = advertised_decode_fixture(
        format!("{} refs/heads/main\n", hex(&id)).into_bytes(),
        &bytes,
    );
    assert_eq!(
        decode_report(&report, &bytes).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn repeated_advertised_peel_claims_consume_one_cumulative_budget() {
    use super::graph_tests::{hex, oid, pack_bytes};
    use GitObjectKind as K;
    let blob = b"x".to_vec();
    let blob_id = oid(K::Blob, &blob);
    let mut id = blob_id;
    let mut objects = vec![(K::Blob, blob)];
    for level in 0..33 {
        let kind = if level == 0 { "blob" } else { "tag" };
        let tag = format!("object {}\ntype {kind}\ntag level{level}\n\n", hex(&id)).into_bytes();
        id = oid(K::Tag, &tag);
        objects.push((K::Tag, tag));
        if level >= 31 {
            let bytes = pack_bytes(&objects);
            let refs = (0..512)
                .map(|i| format!("{} refs/tags/alias{i} peeled:{}\n", hex(&id), hex(&blob_id)))
                .collect::<String>()
                .into_bytes();
            let report = advertised_decode_fixture(refs, &bytes);
            let result = decode_report(&report, &bytes);
            if level == 31 {
                assert_eq!(result.unwrap().graph().reachable_objects(), 33);
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
        }
    }
}

#[test]
fn advertised_report_retains_roots_for_required_object_coverage() {
    use super::graph_tests::{commit, hex, oid, pack_bytes, tree};
    use GitObjectKind as K;
    let blob = b"linked".to_vec();
    let id = oid(K::Blob, &blob);
    let stray = b"stray".to_vec();
    let stray_id = oid(K::Blob, &stray);
    let t = tree("100644", id);
    let tree_id = oid(K::Tree, &t);
    let c = commit(tree_id, &[]);
    let commit_id = oid(K::Commit, &c);
    let bytes = pack_bytes(&[
        (K::Blob, blob),
        (K::Blob, stray),
        (K::Tree, t),
        (K::Commit, c),
    ]);
    let report =
        advertised_decode_fixture(format!("{} HEAD\n", hex(&commit_id)).into_bytes(), &bytes);
    let checked = decode_report(&report, &bytes).unwrap();
    let cancel = CancellationHandle::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let coverage = checked
        .check_required_objects(
            &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
            &[(id, K::Blob), (commit_id, K::Commit)],
            &cancel,
            deadline,
        )
        .unwrap();
    assert_eq!(coverage.required_object_count(), 2);
    assert_eq!(coverage.graph().reachable_objects(), 3);
    assert_eq!(
        checked
            .check_required_objects(
                &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
                &[(stray_id, K::Blob)],
                &cancel,
                deadline
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(checked
        .check_required_objects(
            &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
            &[],
            &cancel,
            deadline
        )
        .is_err());
    assert!(checked
        .check_required_objects(
            &crate::RepositoryUrl::parse("https://github.com/example/repo.git").unwrap(),
            &[(id, K::Tree)],
            &cancel,
            deadline
        )
        .is_err());
}

#[test]
fn transfer_digest_follows_short_writes_and_interruptions() {
    struct ShortSink {
        bytes: Vec<u8>,
        interrupted: bool,
    }
    impl io::Write for ShortSink {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = input.len().min(2);
            self.bytes.extend_from_slice(&input[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("transport must not flush an arbitrary sink")
        }
    }
    let (transport, server) = pack_fixture(pack_body(b"PACKopaque fixture bytes"));
    let mut sink = ShortSink {
        bytes: Vec::new(),
        interrupted: false,
    };
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(report.is_complete_transfer());
    assert_eq!(sink.bytes, b"PACKopaque fixture bytes");
    let digest_hex: String = report
        .accepted_pack_sha256()
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest_hex,
        "ed2cf8c3289ae21bfcc63b511e4e7cb82b50e6b0cd67c43bf0f6903bb1a07433"
    );
}

#[test]
fn decoder_rejects_same_length_valid_pack_substitution_with_same_roots() {
    use super::graph_tests::{hex, oid, pack_bytes};
    use GitObjectKind as K;
    let root = b"root".to_vec();
    let id = oid(K::Blob, &root);
    let original = pack_bytes(&[(K::Blob, root.clone()), (K::Blob, b"x".to_vec())]);
    let substituted = pack_bytes(&[(K::Blob, root), (K::Blob, b"y".to_vec())]);
    assert_eq!(original.len(), substituted.len());
    assert_ne!(original, substituted);
    let alternative = decode_sha1_pack(
        &substituted,
        4096,
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert!(check_pack_graph_closure(
        &alternative,
        &[(id, K::Blob)],
        &CancellationHandle::new(),
        Instant::now() + Duration::from_secs(5)
    )
    .is_ok());
    let report = advertised_decode_fixture(
        format!("{} refs/tags/root\n", hex(&id)).into_bytes(),
        &original,
    );
    assert!(decode_report(&report, &original).is_ok());
    let error = decode_report(&report, &substituted).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "pack bytes do not match transfer digest");
}

#[test]
fn transfer_decode_and_coverage_require_the_expected_repository_context() {
    use super::graph_tests::{hex, oid, pack_bytes};
    use GitObjectKind as K;
    let payload = b"x".to_vec();
    let id = oid(K::Blob, &payload);
    let bytes = pack_bytes(&[(K::Blob, payload)]);
    let report = advertised_decode_fixture(
        format!("{} refs/tags/blob\n", hex(&id)).into_bytes(),
        &bytes,
    );
    let expected = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
    assert_eq!(report.repository(), &expected);
    report.check_context(&expected).unwrap();
    let checked = decode_report(&report, &bytes).unwrap();
    assert_eq!(checked.repository(), &expected);
    checked.check_context(&expected).unwrap();
    let wrong = RepositoryUrl::parse("https://github.com/other/repo.git").unwrap();
    assert_eq!(
        report.check_context(&wrong).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        decode_advertised_pack(
            &report,
            &wrong,
            &bytes,
            4096,
            &CancellationHandle::new(),
            Instant::now() + Duration::from_secs(5)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        checked.check_context(&wrong).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        checked
            .check_required_objects(
                &wrong,
                &[(id, K::Blob)],
                &CancellationHandle::new(),
                Instant::now() + Duration::from_secs(5)
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
