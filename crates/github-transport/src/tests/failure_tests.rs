use super::*;

const SENSITIVE: &str = "https://fixture.invalid/private?token=fixture-secret";

fn observe_status(status: u16, on_post: bool) -> RefObservationReport {
    let tls = fixture_tls();
    let (listener, address) = listener();
    let server = thread::spawn(move || {
        for request_index in 0..=usize::from(on_post) {
            let socket = accept_until(&listener).unwrap();
            let mut stream = tls_stream(socket, Arc::clone(&tls.server)).unwrap();
            let (method, target, _, _) = read_request(&mut stream).unwrap();
            let is_post = request_index == 1;
            assert_eq!(method, if is_post { "POST" } else { "GET" });
            assert_eq!(
                target,
                if is_post {
                    "/example/repo.git/git-upload-pack"
                } else {
                    "/example/repo.git/info/refs?service=git-upload-pack"
                }
            );
            let response_status = if on_post && !is_post { 200 } else { status };
            let content_type = if is_post {
                "application/x-git-upload-pack-result"
            } else {
                "application/x-git-upload-pack-advertisement"
            };
            let body = if is_post {
                let mut body =
                    ref_packet(b"0123456789abcdef0123456789abcdef01234567 refs/heads/main\n");
                body.extend_from_slice(b"0000");
                body
            } else {
                ref_caps(&[b"ls-refs\n"])
            };
            let mut response = format!("HTTP/1.1 {response_status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Fixture-Secret: {SENSITIVE}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
            response.extend_from_slice(&body);
            stream.write_all(&response).unwrap();
            stream.flush().unwrap();
        }
    });
    let transport = transport_at(address, tls.trusted, AttemptLimits::default());
    let report = transport.observe_refs().unwrap();
    server.join().unwrap();
    report
}

#[test]
fn only_status_200_can_complete_smart_http_observation() {
    for on_post in [false, true] {
        for status in [201, 202, 204, 206, 304] {
            let report = observe_status(status, on_post);
            assert_eq!(
                report.operation_error_kind(),
                Some(io::ErrorKind::InvalidData),
                "status {status}, post={on_post}"
            );
            assert!(!report.is_complete());
            assert!(report.refs().is_none());
            assert_eq!(report.finalization().framing().failed(), 1);
            assert_eq!(
                report.finalization().framing().completed(),
                usize::from(on_post)
            );
        }
    }
    assert!(observe_status(200, true).is_complete());
}

#[test]
fn access_denials_keep_permission_kind_through_complete_facade() {
    for on_post in [false, true] {
        for status in [401, 403] {
            let report = observe_status(status, on_post);
            assert_eq!(
                report.operation_error_kind(),
                Some(io::ErrorKind::PermissionDenied),
                "status {status}, post={on_post}"
            );
            assert!(!report.is_complete());
            assert!(report.refs().is_none());
            assert!(report.finalization().observed_ingress_bytes() > 0);
            assert!(!format!("{report:?}").contains(SENSITIVE));
        }
    }
}

#[test]
fn timed_out_headers_keep_timeout_kind_through_complete_facade() {
    let tls = fixture_tls();
    let (listener, address) = listener();
    let server = thread::spawn(move || {
        let socket = accept_until(&listener).unwrap();
        let mut stream = tls_stream(socket, tls.server).unwrap();
        read_request(&mut stream).unwrap();
        // Withhold headers until the candidate closes after its own deadline.
        let _ = stream.read(&mut [0]);
    });
    let transport = transport_at(
        address,
        tls.trusted,
        AttemptLimits::new(1024 * 1024, Duration::from_secs(1)).unwrap(),
    );
    let report = transport.observe_refs().unwrap();
    server.join().unwrap();
    assert_eq!(report.operation_error_kind(), Some(io::ErrorKind::TimedOut));
    assert_eq!(
        report.finalization().drain_error_kind(),
        Some(io::ErrorKind::TimedOut)
    );
    assert!(report.refs().is_none());
}

#[test]
fn socket_error_mapping_keeps_kind_but_discards_context() {
    let token = CancellationHandle::new();
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::TimedOut,
        io::ErrorKind::InvalidData,
    ] {
        let mapped = request_io_error(
            &token,
            io::Error::new(kind, SENSITIVE),
            "request unavailable",
        );
        assert_eq!(mapped.kind(), kind);
        assert_eq!(mapped.to_string(), "request unavailable");
        assert!(!format!("{mapped:?}").contains(SENSITIVE));
    }
    token.cancel();
    assert_eq!(
        request_io_error(
            &token,
            io::Error::new(io::ErrorKind::TimedOut, SENSITIVE),
            "request unavailable"
        )
        .kind(),
        io::ErrorKind::ConnectionAborted
    );
}
