# Security-scoped bookmark feasibility probe

This development-only Swift helper creates one temporary folder and fixture
file per invocation. It accepts no paths or arguments. It compares ordinary
bookmark creation/resolution with a read-only security-scoped bookmark, writes
any successfully created scoped bookmark bytes to a second fixture file and
resolves the persisted bytes,
validates the fixture's volume/resource identity and SHA-256, rejects malformed
bookmark bytes, and simulates replacement of the original path by a different
self-created file.

Build and run with Apple's system Swift toolchain:

```sh
/usr/bin/swiftc -module-cache-path /private/tmp/disk-steward-swift-module-cache \
  -o /private/tmp/disk-steward-bookmark-probe probes/bookmark/BookmarkProbe.swift
/private/tmp/disk-steward-bookmark-probe
```

Observed on macOS 27.0 / Apple Silicon / Swift 6.4: ordinary bookmark creation
and resolution succeeded, the bookmark was not stale, and both resource
identity and fixture bytes matched. Malformed bookmark data was rejected. A
new file created at the original path had a different identity from the moved
original fixture. Read-only security-scoped bookmark creation was unsupported
in this unsigned command-line run: Foundation reported that
`ScopedBookmarksAgent` did not return an error domain. The scoped bookmark was
not created or persisted, so no security-scoped resolution or access result
was available.

The scoped-bookmark error is not evidence that signed/sandboxed app behavior
will fail. Conversely, ordinary bookmark resolution or a successful
`startAccessingSecurityScopedResource()` call alone would not prove sandbox
authorization. This probe does not use `NSOpenPanel`, request UI permission,
read broad user paths, change global configuration, persist/resolve across an
app restart, or exercise an entitled packaged application. It establishes
bookmark mechanics only, not the selected-folder feature or production scope
authorization.
