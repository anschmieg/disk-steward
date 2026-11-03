use super::*;

pub(super) fn pack_fixture(body: Vec<u8>) -> (GitHubTransport, thread::JoinHandle<()>) {
    pack_fixture_with_framing(body, false)
}

fn pack_fixture_with_framing(
    body: Vec<u8>,
    truncated_http: bool,
) -> (GitHubTransport, thread::JoinHandle<()>) {
    let tls = fixture_tls();
    let (listener, address) = listener();
    let config = Arc::clone(&tls.server);
    let server = thread::spawn(move || {
        let caps = ref_caps(&[b"ls-refs\n", b"fetch\n", b"object-format=sha1\n"]);
        let mut refs = ref_packet(b"0123456789abcdef0123456789abcdef01234567 HEAD\n");
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

fn pack_body(payload: &[u8]) -> Vec<u8> {
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
    assert_eq!(sink, b"PACKopaque fixture bytes");
    assert_eq!(report.bytes_written(), sink.len() as u64);
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
    assert!(!report.finalization().is_complete());
}
