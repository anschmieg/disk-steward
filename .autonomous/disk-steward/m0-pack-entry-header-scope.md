# M0 bounded pack-entry header parser

Inactive backend, pure slice parsing of one SHA-1 pack entry's type, declared inflated representation size, and optional offset/reference delta base. Read at most 30 header bytes, allocate nothing, use checked arithmetic and a caller-selected size ceiling no greater than 2 GiB. Reject reserved types, truncated/overflowing fields, zero reference bases, and offset bases outside the preceding object area. Leave compressed tail untouched.

A delta's declared size describes its inflated delta instructions, not reconstructed object size. Offset validation does not prove an object boundary; reference bases may be absent or external. This stage does not decompress, validate object IDs/counts, resolve deltas, establish reachability/completeness, or authorize cleanup. Cancellation/deadlines belong to the caller around this finite in-memory parser; later blocking/decompression stages still need budgets and an outer worker.

Branch atop checksum84b9f7e/main3dc7190. Luna owns new parser module; root reviews, wires API, adds independent adversarial vectors and real-Git first-entry correspondence, runs formatting/tests/Clippy/version checks. Preserve local progress and watches; no app/renderer/FS/network interfaces, no releases, versions 0.0.x, no concurrent builds. Primary format: https://git-scm.com/docs/gitformat-pack.
