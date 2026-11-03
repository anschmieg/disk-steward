# M0 bounded SHA-1 pack checksum stage

Inactivebackend pre-inflationcheck: check_sha1_pack_checksum(reader:&mutimplRead, expected_len:u64, cancellation:&CancellationHandle, deadline:Instant)->ioResult<PackChecksumReport>. Exactlen32..=2GiB, fixed12bytePACK/version2or3/declaredcount header, streamhashheader+compressedbody excluding20bytetrailer withpinnedsha1-checked0.10.0 collisiondetection (rejectcollision regardlesshash), exacttrailercomparison, onebyteEOFlookahead rejectsappendedbytes. Atmostlen+1reads/fixed8KiBbuffer/no allocation/decompression/objectenumeration/perheadercountalloc; checkcancel/deadlinebefore+afterreads/hash, boundInterruptedretries, impossibleReadcountsrefused/errorssanitized. GenericReadmayblockrequiresouterworker; no paths/renderer/shell/credential/networkintegration.

Reportonlyheaderclaims/bytecount/checksum match NOTobjectintegrity/countcorrectness/authenticity/graphcompleteness/reachability/retention/cleanupauthority/milestonepass. Correctlychecksummedmalformedobjectpayloadmaypass; independentobjectdecoder/verifier remainsrequired.

PrimaryGitpackformat reviewed https://git-scm.com/docs/gitformat-pack (SHA1legacyheader12/trailer20/version2or3); RustCryptoSHA1docsrecommendscollisioncheckedcrate, exactcached0.10.0source/APIreviewed. No handwrittenhashimplementation.

Luna mediumnewchecksum.rs only/rootAPIwiring/adversarialreader tests+realGitpack/corrupttruncatedvalidation/docs/dependency/lock/integration/source reviewbeforeexec. Branch atop60166e8f7/main3dc7190baseline/localnotes preserved/existing48–60watchesnotpolled/nooverlappingbuilds/CARGO_INCREMENTAL=0/2GiBreserve/host6.5GiBfree/version0.0.17/no releases.
