# M0 gix synthetic credential injection

Purpose: exercise the pinned gix HTTP transport's set_identity seam with an explicitly supplied synthetic Account. Local loopback server checks expected Authorization internally for actual advertisement GET and upload-pack POST; anonymous paired case proves no header, and a 401 remains a failed handshake. Neither header nor account material should enter assertion output. Requests/fixtures are bounded and loopback only.

No remote requests, real credentials, credential helpers, Keychain queries, renderer access or production authentication API. The supplied invalid fixture identity is not a token. The URL contains no userinfo; the fixture's HTTPS-labelled logical URL is routed by the test backend to plain loopback, so no TLS security claim follows. Do not claim secure secret lifetime/zeroization, revocation, private GitHub access, or M0 acceptance.

Branch from PR34 head3632632; PR33 updated24c907e after main merge, watches33/34 remain untouched. Preserve local32 integration checkpoint. Root reviews concrete code before tests. Require targeted behavioral tests/strictClippy/fmt/version/diff and current-head CI before main merge; version0.0.17. No overlap with builds/agents.
