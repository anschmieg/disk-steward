# Bounded HTTPS reachability probe (0.0.1)

Development-only Python standard-library probe for a GitHub smart-HTTP
`info/refs?service=git-upload-pack` endpoint. Networking is off unless the
caller explicitly passes `allow_network=True`; tests inject a fake response
transport and make no network connections. A caller may supply a token, which
is used only to construct an Authorization header. This probe does not read
Keychain or any other credential store.

The endpoint validator permits HTTPS on `github.com` only, a single
`owner/repository.git` path, and the upload-pack advertisement query. Embedded
credentials and redirects are rejected. The response is bounded by byte count
and a wall-clock deadline; oversized declared and streamed bodies, truncation,
unsupported endpoints, and transport failures fail with sanitized messages.

The streaming deadline is checked after every read, including EOF, and urllib's
single-read operation uses the remaining socket timeout to limit slow-drip
reads. Python's synchronous DNS resolution and connection establishment are not
a hard process-level deadline guarantee on every platform. A production
adapter must enforce a hard outer deadline by isolating and terminating the
transport worker, and must account for DNS/connect cancellation behavior.

Scope is HTTP reachability and bounded response streaming only. It does **not**
prove Git object transfer, clone integrity, private-credential-store validation,
revocation handling, repository safety, verification budgets/storage lifecycle,
or production readiness. It does not invoke Git, clone, or write source data.
