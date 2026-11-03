mod budget_file;
use super::*;
use budget_file::BudgetFileSink;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::PathBuf,
    process::{Child, ExitStatus},
    sync::mpsc,
};

const GIT: &str = "/usr/bin/git";
const MAX_GIT_IN: usize = 512;
const MAX_GIT_OUT: usize = 65_536;
const MAX_PACK: usize = 512;
// Setup and receiver verification run outside the HTTP attempt. Shared CI can
// take longer to start a cold system Git; keep RPC supervision at three seconds.
const GIT_SETUP_DEADLINE: Duration = Duration::from_secs(10);
const GIT_RPC_DEADLINE: Duration = Duration::from_secs(3);
const AUTHOR_DATE: &str = "2001-02-03T04:05:06+0000";

struct PrivateRoot {
    path: PathBuf,
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
}

impl PrivateRoot {
    fn create() -> io::Result<Self> {
        let parent = fs::canonicalize(std::env::temp_dir())?;
        let mut path = None;
        for nonce in 0..100u32 {
            let candidate = parent.join(format!(
                "disk-steward-git-interop-{}-{nonce}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => {
                    path = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let path = path
            .ok_or_else(|| safe_error(io::ErrorKind::AlreadyExists, "fixture root unavailable"))?;
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_dir() || meta.mode() & 0o7777 != 0o700 {
            return Err(safe_error(
                io::ErrorKind::PermissionDenied,
                "fixture root identity invalid",
            ));
        }
        Ok(Self {
            path,
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
        })
    }

    fn repo(&self) -> PathBuf {
        self.path.join("repo.git")
    }

    fn identity_matches(&self) -> io::Result<bool> {
        let meta = fs::symlink_metadata(&self.path)?;
        Ok(meta.file_type().is_dir()
            && meta.dev() == self.dev
            && meta.ino() == self.ino
            && meta.uid() == self.uid
            && meta.mode() & 0o7777 == self.mode)
    }
}

impl Drop for PrivateRoot {
    fn drop(&mut self) {
        if self.identity_matches().unwrap_or(false) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn git_command(root: &PrivateRoot, args: &[&str], protocol: Option<&str>) -> Command {
    let mut command = Command::new(GIT);
    command
        .args(args)
        .current_dir(&root.path)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Disk Steward Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Disk Steward Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", AUTHOR_DATE)
        .env("GIT_COMMITTER_DATE", AUTHOR_DATE);
    if let Some(protocol) = protocol {
        command.env("GIT_PROTOCOL", protocol);
    }
    command
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn finish_child(child: &mut Child, start: Instant, deadline: Duration) -> io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() >= deadline {
            let _ = child.kill();
            let status = child.wait()?;
            return Err(safe_error(
                io::ErrorKind::TimedOut,
                if status.success() {
                    "Git deadline"
                } else {
                    "Git deadline; child reaped"
                },
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn run_git(
    root: &PrivateRoot,
    args: &[&str],
    input: &[u8],
    protocol: Option<&str>,
) -> io::Result<GitOutput> {
    if input.len() > MAX_GIT_IN {
        return Err(safe_error(
            io::ErrorKind::InvalidInput,
            "Git fixture input too large",
        ));
    }
    if !root.identity_matches()? {
        return Err(safe_error(
            io::ErrorKind::PermissionDenied,
            "fixture root changed",
        ));
    }
    let mut command = git_command(root, args, protocol);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let start = Instant::now();
    let mut child = command.spawn()?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(safe_error(io::ErrorKind::Other, "Git stdout unavailable"));
        }
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("git-interop-stdout".into())
        .spawn(move || {
            let mut stdout = stdout;
            let mut bytes = Vec::with_capacity(MAX_GIT_OUT + 1);
            let mut buffer = [0u8; 4096];
            let mut failed = false;
            loop {
                let room = (MAX_GIT_OUT + 1 - bytes.len()).min(buffer.len());
                match stdout.read(&mut buffer[..room]) {
                    Ok(0) => break,
                    Ok(n) => {
                        bytes.extend_from_slice(&buffer[..n]);
                        if bytes.len() > MAX_GIT_OUT {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            let _ = tx.send((bytes, failed));
        });
    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let write_result = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(input))
        .unwrap_or_else(|| Err(safe_error(io::ErrorKind::Other, "Git stdin unavailable")));
    if write_result.is_err() {
        let _ = child.kill();
    }
    let deadline = if protocol.is_some() {
        GIT_RPC_DEADLINE
    } else {
        GIT_SETUP_DEADLINE
    };
    let status_result = finish_child(&mut child, start, deadline);
    if status_result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let join_result = reader.join();
    let output = rx.try_recv();
    write_result?;
    let status = status_result?;
    if join_result.is_err() {
        return Err(safe_error(io::ErrorKind::Other, "Git stdout reader failed"));
    }
    let (stdout, read_failed) =
        output.map_err(|_| safe_error(io::ErrorKind::TimedOut, "Git stdout reader deadline"))?;
    if read_failed || stdout.len() > MAX_GIT_OUT {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "Git stdout limit or read failure",
        ));
    }
    Ok(GitOutput { status, stdout })
}

fn checked_git(root: &PrivateRoot, args: &[&str], input: &[u8]) -> io::Result<Vec<u8>> {
    let result = run_git(root, args, input, None)?;
    if !result.status.success() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "Git fixture command failed",
        ));
    }
    Ok(result.stdout)
}

fn object_id(bytes: &[u8]) -> io::Result<String> {
    if bytes.len() != 41
        || bytes[40] != b'\n'
        || !bytes[..40]
            .iter()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "Git fixture object ID malformed",
        ));
    }
    Ok(std::str::from_utf8(&bytes[..40]).unwrap().to_owned())
}

fn make_repository(root: &PrivateRoot) -> io::Result<(String, String, String, String, String)> {
    let path = root.repo();
    let path_arg = path
        .to_str()
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "fixture path encoding"))?;
    checked_git(
        root,
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            "--object-format=sha1",
            path_arg,
        ],
        b"",
    )?;
    let blob = object_id(&checked_git(
        root,
        &["-C", path_arg, "hash-object", "-w", "--stdin"],
        b"fetch fixture payload\n",
    )?)?;
    let tree = object_id(&checked_git(
        root,
        &["-C", path_arg, "mktree"],
        format!("100644 blob {blob}\tfixture.txt\n").as_bytes(),
    )?)?;
    let commit = object_id(&checked_git(
        root,
        &["-C", path_arg, "commit-tree", &tree, "-m", "fixture commit"],
        b"",
    )?)?;
    checked_git(
        root,
        &["-C", path_arg, "update-ref", "refs/heads/main", &commit],
        b"",
    )?;
    let tag_object = object_id(&checked_git(root, &["-C", path_arg, "mktag"], format!("object {commit}\ntype commit\ntag fixture\ntagger Disk Steward Fixture <fixture@example.invalid> 946684800 +0000\n\nfixture tag\n").as_bytes())?)?;
    checked_git(
        root,
        &[
            "-C",
            path_arg,
            "update-ref",
            "refs/tags/fixture",
            &tag_object,
        ],
        b"",
    )?;
    Ok((path_arg.to_owned(), blob, tree, commit, tag_object))
}

fn create_receiver(root: &PrivateRoot, name: &str) -> io::Result<PathBuf> {
    let path = root.path.join(name);
    let path_arg = path
        .to_str()
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "receiver path encoding"))?;
    checked_git(
        root,
        &[
            "init",
            "--bare",
            "--template=",
            "--object-format=sha1",
            path_arg,
        ],
        b"",
    )?;
    Ok(path)
}

fn receiver_path_arg(path: &std::path::Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| safe_error(io::ErrorKind::InvalidInput, "receiver path encoding"))
}

fn packet_line(data: &str) -> String {
    format!("{:04x}{data}\n", data.len() + 1 + 4)
}

fn candidate_transport(
    repository: RepositoryUrl,
    tls: &FixtureTls,
    address: std::net::SocketAddr,
) -> GitHubTransport {
    let limits = AttemptLimits::new(16 * 1024 * 1024, Duration::from_secs(10)).unwrap();
    let raw = RawHttp::for_test(
        repository.clone(),
        limits,
        Arc::clone(&tls.trusted),
        address,
        "localhost",
    )
    .unwrap();
    assemble(repository, raw, None).unwrap()
}

fn read_v2_request(stream: &mut FixtureStream) -> io::Result<(String, String, Vec<u8>)> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= MAX_HEADERS {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "fixture headers too large",
            ));
        }
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let text = std::str::from_utf8(&header)
        .map_err(|_| safe_error(io::ErrorKind::InvalidData, "fixture headers invalid"))?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_ascii_whitespace();
    let method = first.next().unwrap_or("").to_owned();
    let target = first.next().unwrap_or("").to_owned();
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid fixture request line",
        ));
    }
    let protocols: Vec<_> = text
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .filter(|(n, _)| n.eq_ignore_ascii_case("git-protocol"))
        .collect();
    if protocols.len() != 1 || protocols[0].1.trim() != "version=2" {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "Git protocol v2 header missing",
        ));
    }
    let lengths: Vec<_> = text
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .collect();
    if lengths.len() > 1 {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "duplicate content length",
        ));
    }
    let length = match lengths.first() {
        Some((_, value)) => value
            .trim()
            .parse::<usize>()
            .map_err(|_| safe_error(io::ErrorKind::InvalidData, "invalid content length"))?,
        None => 0,
    };
    if length > MAX_GIT_IN {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "fixture body too large",
        ));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    Ok((method, target, body))
}

fn git_upload_pack(root: &PrivateRoot, args: &[&str], input: &[u8]) -> io::Result<Vec<u8>> {
    let result = run_git(root, args, input, Some("version=2"))?;
    if !result.status.success() {
        return Err(safe_error(io::ErrorKind::InvalidData, "upload-pack failed"));
    }
    Ok(result.stdout)
}

#[test]
fn real_git_refs_and_fetch_verify_objects_in_isolated_receiver() {
    let root = PrivateRoot::create().unwrap();
    let (repo, blob, tree, commit, tag_object) = make_repository(&root).unwrap();
    let peeled = object_id(
        &checked_git(
            &root,
            &["-C", &repo, "rev-parse", "refs/tags/fixture^{}"],
            b"",
        )
        .unwrap(),
    )
    .unwrap();
    let tls = fixture_tls();
    let (listener, address) = listener();
    let server_tls = tls.server.clone();
    let fetch_repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
    let fetch_transport = candidate_transport(fetch_repository, &tls, address);
    let mut pack_sink = BudgetFileSink::new(&root, fetch_transport.cancellation_handle()).unwrap();
    let expected_commit = commit.clone();
    let expected_tag = tag_object.clone();
    let server = thread::spawn(move || -> io::Result<PrivateRoot> {
        for index in 0..5 {
            let socket = accept_until(&listener)?;
            let mut stream = tls_stream(socket, Arc::clone(&server_tls))?;
            stream.sock.deadline = Instant::now() + Duration::from_secs(10);
            let (method, target, body) = read_v2_request(&mut stream)?;
            let (ct, response) = if index == 0 || index == 2 {
                if method != "GET"
                    || target != "/example/repo.git/info/refs?service=git-upload-pack"
                {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "unexpected fixture GET",
                    ));
                }
                (
                    "application/x-git-upload-pack-advertisement",
                    git_upload_pack(
                        &root,
                        &["upload-pack", "--stateless-rpc", "--advertise-refs", &repo],
                        b"",
                    )?,
                )
            } else {
                if method != "POST" || target != "/example/repo.git/git-upload-pack" {
                    return Err(safe_error(
                        io::ErrorKind::InvalidData,
                        "unexpected fixture POST",
                    ));
                }
                if index < 4 {
                    if body != b"0014command=ls-refs\n00010009peel\n000csymrefs\n0000" {
                        return Err(safe_error(
                            io::ErrorKind::InvalidData,
                            "unexpected fixture ls-refs",
                        ));
                    }
                } else {
                    let mut wants = vec![expected_commit.clone(), expected_tag.clone()];
                    wants.sort();
                    let mut expected = packet_line("command=fetch");
                    expected.push_str("0001");
                    for oid in wants {
                        expected.push_str(&packet_line(&format!("want {oid}")));
                    }
                    for arg in ["ofs-delta", "no-progress", "done"] {
                        expected.push_str(&packet_line(arg));
                    }
                    expected.push_str("0000");
                    if body != expected.as_bytes() {
                        return Err(safe_error(
                            io::ErrorKind::InvalidData,
                            "unexpected fixture fetch",
                        ));
                    }
                }
                (
                    "application/x-git-upload-pack-result",
                    git_upload_pack(&root, &["upload-pack", "--stateless-rpc", &repo], &body)?,
                )
            };
            write_response(&mut stream, ct, &response)?;
        }
        Ok(root)
    });
    let repository = RepositoryUrl::parse("https://github.com/example/repo.git").unwrap();
    let report_result = candidate_transport(repository.clone(), &tls, address).observe_refs();
    let fetch_result = fetch_transport.download_advertised_pack(&mut pack_sink);
    let root = server.join().expect("fixture server panicked").unwrap();
    let report = report_result.unwrap();
    let fetch_report = fetch_result.unwrap();
    pack_sink.sync_and_observe().unwrap();
    let pack = pack_sink.contents().unwrap();
    let pack_metadata = fs::metadata(root.path.join("pack.download")).unwrap();
    assert!(
        pack_sink.bytes_charged()
            >= pack_metadata
                .len()
                .max(pack_metadata.blocks().checked_mul(512).unwrap())
    );
    assert!(fetch_report.is_complete_transfer(), "{fetch_report:?}");
    assert_eq!(fetch_report.bytes_written(), pack.len() as u64);
    assert!(
        report.is_complete(),
        "real Git v2 refs exchange did not complete: {report:?}"
    );
    let refs = report.refs().unwrap();
    let head = refs.iter().find(|r| r.name() == b"HEAD").expect("HEAD ref");
    assert_eq!(head.oid(), &hex_oid(&commit));
    assert_eq!(head.symref_target(), Some(&b"refs/heads/main"[..]));
    let main = refs
        .iter()
        .find(|r| r.name() == b"refs/heads/main")
        .expect("main ref");
    assert_eq!(main.oid(), &hex_oid(&commit));
    let tag = refs
        .iter()
        .find(|r| r.name() == b"refs/tags/fixture")
        .expect("tag ref");
    assert_eq!(tag.oid(), &hex_oid(&tag_object));
    assert_eq!(tag.peeled(), Some(&hex_oid(&peeled)));
    assert_eq!(report.finalization().framing().completed(), 2);

    assert!(
        pack.len() <= MAX_PACK && pack.len() <= MAX_GIT_IN,
        "fixture pack exceeds bounded Git input"
    );
    assert!(fetch_report.finalization().is_complete());
    assert_eq!(fetch_report.finalization().framing().completed(), 3);

    let receiver = create_receiver(&root, "receiver.git").unwrap();
    let receiver_arg = receiver_path_arg(&receiver).unwrap();
    checked_git(
        &root,
        &[
            "--git-dir",
            receiver_arg,
            "index-pack",
            "--strict",
            "--stdin",
        ],
        &pack,
    )
    .unwrap();
    checked_git(
        &root,
        &[
            "--git-dir",
            receiver_arg,
            "fsck",
            "--strict",
            "--full",
            "--no-reflogs",
        ],
        b"",
    )
    .unwrap();
    for object in [&blob, &tree, &commit, &tag_object] {
        checked_git(
            &root,
            &["--git-dir", receiver_arg, "cat-file", "-e", object],
            b"",
        )
        .unwrap();
    }

    let corrupt_receiver = create_receiver(&root, "corrupt-receiver.git").unwrap();
    let corrupt_arg = receiver_path_arg(&corrupt_receiver).unwrap();
    let mut corrupted = pack.clone();
    let checksum = corrupted.last_mut().expect("valid pack has a checksum");
    *checksum ^= 1;
    let corrupted_result = run_git(
        &root,
        &[
            "--git-dir",
            corrupt_arg,
            "index-pack",
            "--strict",
            "--stdin",
        ],
        &corrupted,
        None,
    )
    .unwrap();
    assert!(
        !corrupted_result.status.success(),
        "corrupt pack was accepted"
    );

    let truncated_receiver = create_receiver(&root, "truncated-receiver.git").unwrap();
    let truncated_arg = receiver_path_arg(&truncated_receiver).unwrap();
    let truncated_result = run_git(
        &root,
        &[
            "--git-dir",
            truncated_arg,
            "index-pack",
            "--strict",
            "--stdin",
        ],
        &pack[..pack.len() - 1],
        None,
    )
    .unwrap();
    assert!(
        !truncated_result.status.success(),
        "truncated pack was accepted"
    );
}

fn hex_oid(value: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    out
}

#[test]
fn budgeted_file_refusal_stops_candidate_and_retains_partial_charge() {
    let root = PrivateRoot::create().unwrap();
    let mut body = super::ref_packet(b"packfile\n");
    body.extend(super::ref_packet(b"\x01PACKfixture"));
    body.extend_from_slice(b"0000");
    let (transport, server) = super::pack_tests::pack_fixture(body);
    let mut sink =
        BudgetFileSink::new_with_limit(&root, transport.cancellation_handle(), 5).unwrap();
    let report = transport.download_advertised_pack(&mut sink).unwrap();
    server.join().unwrap();
    assert!(!report.is_complete_transfer());
    assert_eq!(sink.contents().unwrap(), b"PACKf");
    assert_eq!(report.bytes_written(), 5);
    let partial_metadata = fs::metadata(root.path.join("pack.download")).unwrap();
    assert!(
        sink.bytes_charged()
            >= partial_metadata
                .len()
                .max(partial_metadata.blocks().checked_mul(512).unwrap())
    );
    assert!(report.operation_error_kind().is_some());
}
