import CryptoKit
import Foundation

// Development-only scope persistence feasibility probe. No input paths are
// accepted. All file URLs are created beneath this invocation's temp root.
struct FixtureIdentity: Equatable {
    let volume: String
    let resource: String
    let size: Int
    let sha256: String
}

enum ProbeError: Error, CustomStringConvertible {
    case failed(String)

    var description: String {
        switch self {
        case .failed(let message): return message
        }
    }
}

func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw ProbeError.failed(message) }
}

func identity(of url: URL) throws -> FixtureIdentity {
    let values = try url.resourceValues(forKeys: [
        .volumeIdentifierKey, .fileResourceIdentifierKey, .fileSizeKey,
        .isRegularFileKey, .isSymbolicLinkKey
    ])
    guard values.isRegularFile == true,
          values.isSymbolicLink != true,
          let volume = values.volumeIdentifier,
          let resource = values.fileResourceIdentifier,
          let size = values.fileSize else {
        throw ProbeError.failed("fixture resource identity unavailable or not a regular file")
    }
    let digest = SHA256.hash(data: try Data(contentsOf: url, options: .mappedIfSafe))
        .map { String(format: "%02x", $0) }.joined()
    return FixtureIdentity(volume: String(describing: volume),
                           resource: String(describing: resource),
                           size: size, sha256: digest)
}

func resolve(_ data: Data, options: URL.BookmarkResolutionOptions = []) throws -> (URL, Bool) {
    var stale = false
    let url = try URL(resolvingBookmarkData: data, options: options,
                      relativeTo: nil, bookmarkDataIsStale: &stale)
    return (url, stale)
}

func readWithScope(_ url: URL) throws -> (started: Bool, bytes: Data) {
    let started = url.startAccessingSecurityScopedResource()
    defer {
        if started { url.stopAccessingSecurityScopedResource() }
    }
    // On an unsigned command-line process, this read is only a fixture sanity
    // check. A false start result is not interpreted as a sandbox denial.
    return (started, try Data(contentsOf: url))
}

func run() throws {
    guard CommandLine.arguments.count == 1 else {
        throw ProbeError.failed("this helper accepts no arguments or caller-supplied paths")
    }

    let fm = FileManager.default
    let root = fm.temporaryDirectory.appendingPathComponent(
        "disk-steward-bookmark-\(UUID().uuidString)", isDirectory: true)
    try fm.createDirectory(at: root, withIntermediateDirectories: false)
    defer { try? fm.removeItem(at: root) }

    let original = root.appendingPathComponent("bookmark-fixture.txt")
    let payload = Data("bookmark-scope-feasibility-fixture".utf8)
    try payload.write(to: original, options: .withoutOverwriting)
    let initialIdentity = try identity(of: original)

    // Ordinary bookmark: a navigation/reference baseline, without scope flags.
    let ordinaryData = try original.bookmarkData(options: [],
                                                  includingResourceValuesForKeys: nil,
                                                  relativeTo: nil)
    let (ordinaryURL, ordinaryStale) = try resolve(ordinaryData)
    let ordinaryIdentity = try identity(of: ordinaryURL)
    try require(ordinaryIdentity == initialIdentity,
                "ordinary bookmark did not resolve to the self-created fixture identity")
    try require(try Data(contentsOf: ordinaryURL) == payload,
                "ordinary bookmark did not read the expected fixture bytes")

    // Read-only security-scoped bookmark. Do not treat creation/resolution or
    // startAccessing returning true as evidence of sandbox rights in this CLI.
    var scopedCreateResult = "unsupported"
    var scopedRecordPersisted = false
    var scopedResolveResult = "unsupported"
    var scopedStale: Bool?
    var scopedStartAccessing: Bool?
    var scopedIdentityMatches: Bool?
    var scopedReadMatches: Bool?
    var scopedError: String?

    do {
        let scopedData = try original.bookmarkData(
            options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
            includingResourceValuesForKeys: nil,
            relativeTo: nil)
        scopedCreateResult = "created"
        let record = root.appendingPathComponent("read-only-scope-bookmark.data")
        try scopedData.write(to: record, options: .withoutOverwriting)
        let persistedScopedData = try Data(contentsOf: record)
        scopedRecordPersisted = persistedScopedData == scopedData
        let (scopedURL, stale) = try resolve(persistedScopedData, options: [.withSecurityScope])
        scopedResolveResult = "resolved"
        scopedStale = stale
        let resolvedIdentity = try identity(of: scopedURL)
        scopedIdentityMatches = resolvedIdentity == initialIdentity
        let access = try readWithScope(scopedURL)
        scopedStartAccessing = access.started
        scopedReadMatches = access.bytes == payload
    } catch {
        scopedError = String(describing: error)
    }

    // Malformed data must be rejected; no URL or file is provided by a caller.
    var malformedRejected = false
    do {
        _ = try resolve(Data([0x00, 0x01, 0x02, 0x03]))
    } catch {
        malformedRejected = true
    }

    // Simulate reuse of the original path with a new, also self-created file.
    // The original file is moved aside inside the fixture, not deleted.
    let movedOriginal = root.appendingPathComponent("original-object-moved-aside.txt")
    let replacement = root.appendingPathComponent("bookmark-fixture.txt")
    try fm.moveItem(at: original, to: movedOriginal)
    try Data("different-path-reuse-content".utf8).write(to: replacement,
                                                         options: .withoutOverwriting)
    let replacementIdentity = try identity(of: replacement)
    let oldObjectAtMovedPath = try identity(of: movedOriginal)
    let pathReuseChangedIdentity = replacementIdentity != initialIdentity
    try require(pathReuseChangedIdentity,
                "path reuse fixture unexpectedly retained the original identity")
    try require(oldObjectAtMovedPath == initialIdentity,
                "moving the original fixture changed its volume/resource/content identity")

    print("fixture=temporary_self_created")
    print("volume_resource_identity=available")
    print("ordinary_bookmark=created_and_resolved")
    print("ordinary_bookmark_stale=\(ordinaryStale)")
    print("ordinary_bookmark_identity_and_bytes=matched")
    print("security_scoped_bookmark_create=\(scopedCreateResult)")
    print("security_scoped_bookmark_record_persisted=\(scopedRecordPersisted)")
    print("security_scoped_bookmark_resolve=\(scopedResolveResult)")
    print("security_scoped_bookmark_stale=\(scopedStale.map(String.init) ?? "unavailable")")
    print("security_scoped_bookmark_identity_matches=\(scopedIdentityMatches.map(String.init) ?? "unavailable")")
    print("security_scoped_start_accessing_returned=\(scopedStartAccessing.map(String.init) ?? "unavailable")")
    print("security_scoped_fixture_read_matches=\(scopedReadMatches.map(String.init) ?? "unavailable")")
    if let scopedError { print("security_scoped_error=\(scopedError)") }
    print("malformed_bookmark_rejected=\(malformedRejected)")
    print("path_reuse_new_identity_detected=\(pathReuseChangedIdentity)")
    print("limitation=unsigned CLI; no sandbox entitlement, NSOpenPanel grant, app restart, user-selected scope, or broad-path access test")
    print("scope=bookmark feasibility only; not a selected-folder feature or production authorization proof")
}

do {
    try run()
} catch {
    fputs("bookmark_probe_failed=\(error)\n", stderr)
    exit(EXIT_FAILURE)
}
