# Exact-directory exclusion identity v1

This contract defines the native identity observation used by the private,
versioned exclusion registry. It records a remembered directory locator; it
does not provide a scan scope or permission grant. Exclusions are intended for
future scans only and do not alter the synthetic general plan.

## Observation

On macOS, the adapter accepts one absolute directory path selected by native
UI. It preserves the path's raw Unix bytes as a locator, including non-UTF-8
bytes if the filesystem provides them. The native store wraps the complete
binding record in its versioned encoding. The renderer receives none of the
locator bytes. The visible basename is decoded for display, control characters
are removed, bidi controls are escaped, and the result is bounded to 120
characters.

The adapter rejects a symbolic link at the selected leaf and rejects files.
It obtains a read-only directory descriptor with `O_DIRECTORY`, `O_NOFOLLOW`,
and `O_CLOEXEC`. It does not enumerate children, read file contents, request
bookmarks, resolve aliases, or retain an authorization token. It reads the
volume UUID through Foundation's `NSURLVolumeUUIDStringKey` resource value and
requires a canonical UUID string. It requires `NSURLVolumeIsLocalKey` to report
`true`; absent locality data and remote volumes remain unavailable. It reads object identity from metadata for
the opened descriptor: device number, inode number, and creation time.

Before and after the Foundation path query, no-follow path metadata must still
identify the opened directory. The observation is rejected if creation time or
volume identity is unavailable. A fresh observation is required for every
comparison; there is no durable `active` or `matched` bit.

## Private version-1 byte fields

All integer fields use little-endian encoding. No opaque Objective-C object's
debug/display representation is used.

| Field | Encoding |
| --- | --- |
| `locator_bytes` | The exact raw absolute path bytes. The native store wraps the complete binding record in its versioned encoding. |
| `volume_identity` | ASCII tag `DSXVOL\x01`, then the lower-case 36-byte UUID string |
| `directory_identity` | ASCII tag `DSXDIR\x01`, then `u64` device, `u64` inode, `u64` creation seconds since Unix epoch, and `u32` creation nanoseconds |

The basename is display metadata and is not part of the identity comparison.
Private fields must remain in the native exclusion store and must never be
serialized into renderer-facing DTOs or logs.

## Revalidation states

The adapter reports `Matched` only when the locator bytes, volume UUID, and
directory identity all match a fresh observation. A missing path, leaf
replacement, symlink, non-directory, or moved directory reports `Stale`.
Unsupported platforms, denied access, and unavailable birth-time or volume
identity report `Unavailable`. Neither `Stale` nor `Unavailable` authorizes a
match or grants access.

Non-macOS builds explicitly report this adapter as unsupported. A device and
inode pair alone is not treated as a stable cross-remount volume identity.

## Fixture validation boundary

Platform tests may create and modify only disposable temporary directories.
They cover Unicode directory observations, raw non-UTF-8 locator round trips,
same-directory revalidation, symlink/file/missing rejection, moved and replaced
directories, fail-closed unavailable identity handling, identity version tags,
and preservation of a sentinel file. Filesystems that reject non-UTF-8 names
are covered by the raw locator unit test only. Tests do not use user paths or
traverse selected directories.
