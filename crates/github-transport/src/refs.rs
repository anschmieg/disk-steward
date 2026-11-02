use std::{collections::HashSet, io};

use gix_transport::{client::blocking_io::TransportV2Ext, Protocol};

use crate::{check_deadline, safe_error, GitHubTransport};

const MAX_LINE: usize = 4096;
const MAX_NAME: usize = 1024;
const MAX_REFS: usize = 1024;

/// One raw advertised reference. The object ID is fixed-width SHA-1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvertisedRef {
    /// Lossless reference name bytes.
    name: Vec<u8>,
    /// Advertised SHA-1 object ID.
    oid: [u8; 20],
    /// Optional peeled target of an annotated tag.
    peeled: Option<[u8; 20]>,
    /// Optional symref target, retained as raw bytes.
    symref_target: Option<Vec<u8>>,
}
impl AdvertisedRef {
    pub fn name(&self) -> &[u8] {
        &self.name
    }
    pub fn oid(&self) -> &[u8; 20] {
        &self.oid
    }
    pub fn peeled(&self) -> Option<&[u8; 20]> {
        self.peeled.as_ref()
    }
    pub fn symref_target(&self) -> Option<&[u8]> {
        self.symref_target.as_deref()
    }
}

pub(super) fn read_advertised_refs(t: &mut GitHubTransport) -> io::Result<Vec<AdvertisedRef>> {
    let cancellation = t.cancellation_handle();
    let deadline = t.deadline;
    check_deadline(deadline)?;
    cancellation.check()?;
    let response = gix_transport::client::blocking_io::Transport::handshake(
        t,
        gix_transport::Service::UploadPack,
        &[],
    )
    .map_err(|_| {
        cancellation.check().err().unwrap_or_else(|| {
            safe_error(io::ErrorKind::InvalidData, "upload-pack handshake failed")
        })
    })?;
    let protocol = response.actual_protocol;
    let capabilities = response.capabilities.clone();
    drop(response);
    let ls_refs_count = capabilities
        .iter()
        .filter(|cap| cap.name() == "ls-refs")
        .count();
    let format_count = capabilities
        .iter()
        .filter(|cap| cap.name() == "object-format")
        .count();
    let ls_refs = ls_refs_count == 1;
    let sha1 = format_count == 0
        || (format_count == 1
            && capabilities
                .capability("object-format")
                .and_then(|cap| cap.value())
                .map(|v| v == "sha1")
                .unwrap_or(false));
    if protocol != Protocol::V2 || !ls_refs || !sha1 {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "unsupported Git protocol capabilities",
        ));
    }
    let mut reader = t
        .invoke(
            "ls-refs",
            std::iter::empty::<(&str, Option<&str>)>(),
            Some([b"peel".to_vec().into(), b"symrefs".to_vec().into()].into_iter()),
            false,
        )
        .map_err(|_| {
            cancellation
                .check()
                .err()
                .unwrap_or_else(|| safe_error(io::ErrorKind::InvalidData, "ls-refs request failed"))
        })?;
    let mut refs = Vec::new();
    let mut names = HashSet::new();
    loop {
        cancellation.check()?;
        check_deadline(deadline)?;
        let Some(item) = reader.readline() else {
            if reader.stopped_at() == Some(gix_transport::client::MessageKind::Flush) {
                break;
            }
            return Err(safe_error(
                io::ErrorKind::UnexpectedEof,
                "incomplete ls-refs response",
            ));
        };
        let item = item.map_err(|error| safe_error(error.kind(), "invalid ls-refs packet"))?;
        let line = match item {
            Ok(gix_transport::packetline::PacketLineRef::Data(d)) => d,
            _ => {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "invalid ls-refs terminator",
                ))
            }
        };
        let parsed = parse_ref_line(line)?;
        let name = parsed.name();
        if refs.len() == MAX_REFS || !names.insert(name.to_vec()) {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "advertised reference limit or duplicate",
            ));
        }
        refs.push(parsed);
    }
    drop(reader);
    cancellation.check()?;
    check_deadline(deadline)?;
    Ok(refs)
}

fn parse_ref_line(line: &[u8]) -> io::Result<AdvertisedRef> {
    if line.len() > MAX_LINE || !line.ends_with(b"\n") || line[..line.len() - 1].contains(&b'\n') {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid ls-refs line",
        ));
    }
    let fields: Vec<&[u8]> = line[..line.len() - 1].split(|b| *b == b' ').collect();
    if fields.len() < 2
        || fields[1].is_empty()
        || fields[1].len() > MAX_NAME
        || fields[0].is_empty()
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid advertised reference",
        ));
    }
    let name = fields[1];
    if name != b"HEAD"
        && (!name.starts_with(b"refs/")
            || gix_validate::reference::name(gix_transport::bstr::ByteSlice::as_bstr(name))
                .is_err())
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid advertised reference",
        ));
    }
    let oid = parse_oid(fields[0])?;
    let mut peeled = None;
    let mut symref_target = None;
    for attr in &fields[2..] {
        if let Some(value) = attr.strip_prefix(b"peeled:") {
            if peeled.is_some() {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "duplicate reference attribute",
                ));
            }
            peeled = Some(parse_oid(value)?);
        } else if let Some(value) = attr.strip_prefix(b"symref-target:") {
            if symref_target.is_some()
                || value.is_empty()
                || value.len() > MAX_NAME
                || !value.starts_with(b"refs/")
                || gix_validate::reference::name(gix_transport::bstr::ByteSlice::as_bstr(value))
                    .is_err()
            {
                return Err(safe_error(
                    io::ErrorKind::InvalidData,
                    "invalid reference attribute",
                ));
            }
            symref_target = Some(value.to_vec());
        } else {
            return Err(safe_error(
                io::ErrorKind::InvalidData,
                "unsupported reference attribute",
            ));
        }
    }
    Ok(AdvertisedRef {
        name: name.to_vec(),
        oid,
        peeled,
        symref_target,
    })
}

fn parse_oid(value: &[u8]) -> io::Result<[u8; 20]> {
    if value.len() != 40
        || value
            .iter()
            .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(b))
    {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid SHA-1 object ID",
        ));
    }
    let mut out = [0; 20];
    for (i, pair) in value.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (hex(pair[0]) << 4) | hex(pair[1]);
    }
    if out.iter().all(|b| *b == 0) {
        return Err(safe_error(
            io::ErrorKind::InvalidData,
            "invalid SHA-1 object ID",
        ));
    }
    Ok(out)
}
fn hex(b: u8) -> u8 {
    if b <= b'9' {
        b - b'0'
    } else {
        b - b'a' + 10
    }
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    fn oid() -> &'static [u8] {
        b"0123456789abcdef0123456789abcdef01234567"
    }
    fn line(name: &[u8], attrs: &[&[u8]]) -> Vec<u8> {
        let mut out = oid().to_vec();
        out.push(b' ');
        out.extend_from_slice(name);
        for attr in attrs {
            out.push(b' ');
            out.extend_from_slice(attr);
        }
        out.push(b'\n');
        out
    }

    #[test]
    fn parses_head_peeled_symref_and_raw_non_utf8_name() {
        let head = parse_ref_line(&line(b"HEAD", &[b"symref-target:refs/heads/main"])).unwrap();
        assert_eq!(head.name(), b"HEAD");
        assert_eq!(head.symref_target(), Some(&b"refs/heads/main"[..]));
        let tag = parse_ref_line(&line(
            b"refs/tags/v1",
            &[b"peeled:abcdefabcdefabcdefabcdefabcdefabcdefabcd"],
        ))
        .unwrap();
        assert!(tag.peeled().is_some());
        let raw = parse_ref_line(&line(b"refs/heads/non\xffutf8", &[])).unwrap();
        assert_eq!(raw.name(), b"refs/heads/non\xffutf8");
    }

    #[test]
    fn rejects_malformed_ids_names_attributes_and_controls() {
        for bad in [
            b"ABC refs/heads/main\n".as_slice(),
            b"0123456789ABCDEF0123456789ABCDEF01234567 refs/heads/main\n",
            b"0000000000000000000000000000000000000000 refs/heads/main\n",
            b"0123456789abcdef0123456789abcdef01234567 refs/heads/../oops\n",
            b"0123456789abcdef0123456789abcdef01234567 refs/heads/main\0x\n",
        ] {
            assert!(parse_ref_line(bad).is_err());
        }
        assert!(parse_ref_line(&line(
            b"refs/heads/main",
            &[
                b"peeled:0123456789abcdef0123456789abcdef01234567",
                b"peeled:0123456789abcdef0123456789abcdef01234567"
            ]
        ))
        .is_err());
        assert!(parse_ref_line(&line(b"refs/heads/main", &[b"mystery"])).is_err());
        assert!(parse_ref_line(&line(
            b"refs/heads/main",
            &[b"symref-target:refs/heads/../bad"]
        ))
        .is_err());
    }

    #[test]
    fn pre_cancelled_ref_operation_propagates_cancellation_without_network() {
        let mut transport = crate::connect(
            crate::RepositoryUrl::parse("https://github.com/a/b").unwrap(),
            crate::AttemptLimits::default(),
            None,
        )
        .unwrap();
        transport.cancellation_handle().cancel();
        assert_eq!(
            transport.read_advertised_refs().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        let report = transport.finalize_http_framing().unwrap();
        assert_eq!(report.observed_ingress_bytes(), 0);
        assert!(!report.is_complete());
    }

    #[test]
    fn enforces_line_and_name_limits() {
        let mut exact_name = b"refs/heads/".to_vec();
        exact_name.resize(MAX_NAME, b'a');
        assert_eq!(
            parse_ref_line(&line(&exact_name, &[])).unwrap().name(),
            exact_name
        );
        let mut long_name = exact_name;
        long_name.push(b'a');
        assert_eq!(
            parse_ref_line(&line(&long_name, &[])).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut too_long = line(b"refs/heads/main", &[]);
        too_long.pop();
        too_long.extend(std::iter::repeat_n(b'x', MAX_LINE));
        too_long.push(b'\n');
        assert!(parse_ref_line(&too_long).is_err());
        let exact = line(b"refs/heads/main", &[]);
        assert!(parse_ref_line(&exact).is_ok());
    }
}
