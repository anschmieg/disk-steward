# Atomic rename feasibility probe

Workspace development crate, version `0.0.3`. Run its fixture-only checks with:

```sh
cargo test -p atomic-rename-probe --locked
cargo fmt --all --check
cargo clippy -p atomic-rename-probe --all-targets --locked -- -D warnings
```

The crate pins `rustix = 1.1.6` and `tempfile = 3.27.0`. It accepts only a
single component for each name and already-open source/holding directory
handles. Tests create their own `tempfile` trees; there is no arbitrary-path
CLI, production cleanup path, plain-rename fallback, or production cleanup.
`rustix` is a target-specific dependency for Linux and macOS, so unsupported
targets compile the std-only `Unsupported` branch without pulling in Unix APIs.

`rustix::fs::renameat_with(..., RenameFlags::NOREPLACE)` is the safe interface
used for the atomic same-volume move. On Linux rustix implements this through
`renameat2` and `RENAME_NOREPLACE`. On Apple targets rustix maps `NOREPLACE` to
`renameatx_np` with `RENAME_EXCL`; it also handles older macOS where that symbol
may be absent by returning an error. A volume/kernel that rejects the flag is
reported as unsupported; the operation does not retry with an overwriting
rename. See the [rustix 1.1.6 API](https://docs.rs/rustix/1.1.6/rustix/fs/fn.renameat_with.html),
[rustix platform implementation](https://docs.rs/crate/rustix/1.1.6/source/src/backend/libc/fs/syscalls.rs),
and [Apple's `renameatx_np` API reference](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/ToolsandAPIs/ToolsandAPIs.html).

The final component is first opened with `openat` and `O_NOFOLLOW`, rejecting
final symlinks. `O_NONBLOCK` prevents a FIFO open from waiting for a peer, and
the opened identity must be a regular file or directory; sockets and device
files are rejected. Its device/inode identity is checked again by opening the
isolated name after the move. This catches a changed source name when the
replacement has a different identity, but does not eliminate the race where
the source directory entry changes after pre-open and before rename. Production
staging must revalidate identity after isolation and reconcile a mismatch or
ambiguous outcome. Never silently treat it as a successful stage. Opened parent
directory handles and validated single-component names also prevent parent
path traversal in this probe.

## Evidence and limits

- Fixture with an existing destination: no-replace returns a conflict and both
  source and destination contents remain intact.
- Successful stage and restore: device and inode remain equal to the original.
- Final symlink: no-follow open rejects it; link and target remain present.
- A deterministic injected device/inode mismatch returns `OutcomeUncertain`.
  Post-rename validation errors and unexpected rename errors have the same typed
  outcome; callers must reconcile before retrying. Destination conflicts are
  typed separately, while unsupported-flag and cross-volume errors return
  `Unsupported`.
- Native tests were run on macOS, `aarch64-apple-darwin`, with Cargo/rustc
  1.99.1. They cover exclusive destination behavior, inode/device preservation,
  symlink rejection, trailing-separator rejection, and component validation.
  Linux additionally has a FIFO fixture test, but that test was not run in this
  macOS validation pass. Cross-volume failure, crash durability, races under
  adversarial concurrent writers, permissions, and production receipt/recovery
  behavior remain unproven.
- Unsupported operating systems return an explicit `Unsupported` state at
  compile-time-selected code. A conditional unsupported-target test verifies
  the source fixture remains unchanged. On Linux/macOS, runtime unsupported
  errors are surfaced without a mutating fallback.

Integration guidance: treat this as a narrow primitive feasibility result,
not an execution implementation. The production adapter still needs approval
checks, durable receipt journaling, source and post-isolation identity
revalidation, reconciliation after interruption, and tests on each supported
OS/filesystem combination before it can stage user data.
