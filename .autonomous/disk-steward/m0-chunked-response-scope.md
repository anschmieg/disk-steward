# M0 bounded chunked response decoding

Purpose: extend the candidate's fixed-length HTTP responses with bounded HTTP/1.1 chunked decoding above the existing raw TLS counter. One chunked transfer coding only, no simultaneous Content-Length, duplicate coding, compression or coding chains. Sizes must parse without overflow; all framing bytes remain charged below TLS. No allocation from declared chunk sizes.

Chunk lines/extensions and trailer sections are bounded. Parse valid unknown extensions then ignore them; discard bounded well-formed trailers without merging into response headers, rejecting framing/auth/routing fields. Malformed/truncated delimiters, sizes and trailers must remain errors on repeated reads. Payload/control reads retain the same attempt deadline, including already buffered TLS data. See RFC9112 sections6.3/7.1; no claim of general unrestricted HTTP compatibility.

Use actual local gix TLS GET/POST fixtures split across chunks and packet lines, plus adversarial/truncation/cap/deadline cases. Unit build cannot fall back to production DNS. No remote requests, app activation, credentials, new connector/trust surface, stores, cleanup or M0/M4 acceptance. Full-fetch/DNS supervision/trust policy/private-auth gates remain open.

Branch atop PR40 head3be70e0; main final baseline. Bounded Luna owns candidate lib.rs/README only, root owns scope/progress and concrete source review before tests. Version0.0.17, focused behavior/strictClippy/fmt/version/diff/current-headCI required. Existing38/39/40watches notpolled. Preserve local checkpoint; host14GiB observed before work.
