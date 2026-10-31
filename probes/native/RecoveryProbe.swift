import Foundation
import CryptoKit
import Darwin

// Development-only macOS probe. It accepts no paths; its sole optional flag
// selects an exact-item purge experiment using only self-created fixtures. It
// creates every fixture beneath a fresh temporary directory.
struct Identity: Equatable {
    let volume: String
    let resource: String
    let size: Int?
    let modified: String?
    let regularFile: Bool
    let sha256: String?
}

func identity(of url: URL) throws -> Identity {
    var freshURL = URL(fileURLWithPath: url.path)
    freshURL.removeAllCachedResourceValues()
    let values = try freshURL.resourceValues(forKeys: [
        .volumeIdentifierKey, .fileResourceIdentifierKey, .fileSizeKey,
        .contentModificationDateKey, .isSymbolicLinkKey, .isRegularFileKey
    ])
    guard let volume = values.volumeIdentifier,
          let resource = values.fileResourceIdentifier,
          values.isSymbolicLink != true else {
        throw ProbeError.identityUnavailable(url.lastPathComponent)
    }
    let regularFile = values.isRegularFile == true
    let digest: String?
    if regularFile {
        let bytes = try Data(contentsOf: url, options: .mappedIfSafe)
        digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
    } else {
        digest = nil
    }
    return Identity(volume: String(describing: volume),
                    resource: String(describing: resource), size: values.fileSize,
                    modified: values.contentModificationDate.map { ISO8601DateFormatter().string(from: $0) },
                    regularFile: regularFile, sha256: digest)
}

enum ProbeError: Error, CustomStringConvertible {
    case identityUnavailable(String)
    case assertion(String)

    var description: String {
        switch self {
        case .identityUnavailable(let name): return "resource identity unavailable for \(name)"
        case .assertion(let message): return message
        }
    }
}

func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw ProbeError.assertion(message) }
}

func writeFixture(_ contents: String, to url: URL) throws {
    try Data(contents.utf8).write(to: url, options: .withoutOverwriting)
}

func stageSelfCreatedFile(_ original: URL, expected: Identity,
                          fileManager: FileManager) throws -> URL {
    let beforeMutation = try identity(of: original)
    try require(beforeMutation.regularFile && beforeMutation == expected,
                "fixture identity/hash changed before native Trash call")
    var returned: NSURL?
    try fileManager.trashItem(at: original, resultingItemURL: &returned)
    guard let returnedURL = returned as URL? else {
        throw ProbeError.assertion("Foundation returned no receipt URL")
    }
    let receiptIdentity = try identity(of: returnedURL)
    try require(receiptIdentity.regularFile, "returnedURL is not a regular file")
    try require(receiptIdentity == expected, "returnedURL identity/hash does not match fixture")
    return returnedURL
}

func pathEntryExists(_ url: URL) -> Bool {
    var info = stat()
    return url.path.withCString { lstat($0, &info) == 0 }
}

func restoreExactReceipt(_ returnedURL: URL, to original: URL,
                         expected: Identity, fileManager: FileManager) throws {
    let current = try identity(of: returnedURL)
    try require(current.regularFile && current == expected,
                "refusing restore because returnedURL identity/hash changed")
    try require(!pathEntryExists(original), "restore destination is occupied")
    try fileManager.moveItem(at: returnedURL, to: original)
    let restored = try identity(of: original)
    try require(restored.regularFile && restored == expected,
                "restored file identity/hash did not match receipt")
}

func runSelectivePurge(root: URL, fileManager fm: FileManager) throws {
    // Both Trash entries are created by this invocation. The probe never
    // enumerates Trash; it retains only the two returnedURL receipts.
    let target = root.appendingPathComponent("purge-target-self-created.txt")
    let unrelated = root.appendingPathComponent("unrelated-self-created.txt")
    try writeFixture("exact-self-created-purge-target", to: target)
    try writeFixture("self-created-unrelated-trash-sentinel", to: unrelated)
    let targetID = try identity(of: target)
    let unrelatedID = try identity(of: unrelated)

    let targetReceipt = try stageSelfCreatedFile(target, expected: targetID, fileManager: fm)
    var unrelatedReceipt: URL?
    do {
        unrelatedReceipt = try stageSelfCreatedFile(unrelated, expected: unrelatedID, fileManager: fm)
    } catch {
        // Best effort recovery of the first exact receipt if the second stage
        // did not complete. Never delete either item on this path.
        if !pathEntryExists(target) {
            try? restoreExactReceipt(targetReceipt, to: target, expected: targetID, fileManager: fm)
        }
        throw error
    }
    guard let unrelatedReceipt else {
        throw ProbeError.assertion("unrelated fixture did not produce a returnedURL")
    }

    // Requery the exact target returnedURL immediately before the sole purge.
    // There is still a residual path race between this check and removeItem.
    let freshTargetIdentity = try identity(of: targetReceipt)
    try require(freshTargetIdentity.regularFile && freshTargetIdentity == targetID,
                "purge target receipt changed; leaving both Trash fixtures untouched")
    try require(!pathEntryExists(target), "purge fixture original path unexpectedly exists")
    try fm.removeItem(at: targetReceipt)
    try require(!pathEntryExists(targetReceipt), "target returnedURL still exists after exact-item purge")

    // Prove the separately tracked synthetic Trash item survived, then restore
    // only from its returnedURL. No Trash listing or unrelated path is queried.
    let unrelatedNow = try identity(of: unrelatedReceipt)
    try require(unrelatedNow.regularFile && unrelatedNow == unrelatedID,
                "unrelated self-created Trash sentinel changed")
    try restoreExactReceipt(unrelatedReceipt, to: unrelated,
                            expected: unrelatedID, fileManager: fm)
    print("selective_purge=passed (removed exact target returnedURL only)")
    print("unrelated_trash_sentinel=survived_and_restored_by_its_returnedURL")
    print("selective_purge_limit=path-based race remains; no production gate claim")
}

func run(_ selectivePurge: Bool) throws {
    let fm = FileManager.default
    let root = fm.temporaryDirectory.appendingPathComponent("disk-steward-native-\(UUID().uuidString)", isDirectory: true)
    try fm.createDirectory(at: root, withIntermediateDirectories: false)
    // This cleanup is scoped to the fresh root created above. A file moved to
    // native Trash is outside it and is never removed by this cleanup.
    defer { try? fm.removeItem(at: root) }

    print("fixture_root=\(root.path)")

    // App-managed same-volume stage/restore probe.
    let original = root.appendingPathComponent("internal-original.txt")
    let holding = root.appendingPathComponent("holding")
    try fm.createDirectory(at: holding, withIntermediateDirectories: false)
    try writeFixture("internal-stage-fixture", to: original)
    let originalID = try identity(of: original)
    try require(originalID.volume == (try identity(of: holding)).volume,
                "fixture and holding directory unexpectedly use different volumes")
    let staged = holding.appendingPathComponent("receipt-\(UUID().uuidString).item")
    try fm.moveItem(at: original, to: staged)
    try require(try identity(of: staged) == originalID, "identity changed during stage rename")

    // A restore collision must be a hard stop. Both the staged object and the
    // independently created destination object must remain intact.
    try writeFixture("collision-fixture", to: original)
    let collisionID = try identity(of: original)
    var collisionRejected = false
    do {
        guard !fm.fileExists(atPath: original.path) else { throw ProbeError.assertion("restore collision rejected") }
        try fm.moveItem(at: staged, to: original)
    } catch ProbeError.assertion("restore collision rejected") {
        collisionRejected = true
    }
    try require(collisionRejected, "restore did not reject an occupied destination")
    try require(try identity(of: staged) == originalID, "staged identity changed after collision")
    try require(try identity(of: original) == collisionID, "collision fixture changed")
    // Remove only the exact collision fixture created here, after verifying its
    // identity, so the original synthetic item can be restored in this run.
    try require(try identity(of: original) == collisionID, "refusing to remove changed collision fixture")
    try fm.removeItem(at: original)
    guard !fm.fileExists(atPath: original.path) else { throw ProbeError.assertion("collision path remained occupied") }
    try fm.moveItem(at: staged, to: original)
    try require(try identity(of: original) == originalID, "identity changed during restore")
    print("internal_stage_restore=passed")
    print("restore_collision=rejects_and_preserves_both")

    // Same-size in-place edit within one run exercises content identity beyond
    // size, resource ID and coarse modification-time comparisons.
    let editFixture = root.appendingPathComponent("same-size-edit-fixture.txt")
    try writeFixture("AAAA", to: editFixture)
    let beforeEdit = try identity(of: editFixture)
    let handle = try FileHandle(forWritingTo: editFixture)
    try handle.seek(toOffset: 0)
    try handle.write(contentsOf: Data("BBBB".utf8))
    try handle.synchronize()
    try handle.close()
    let afterEdit = try identity(of: editFixture)
    try require(beforeEdit.size == afterEdit.size, "content-edit fixture unexpectedly changed size")
    try require(beforeEdit.sha256 != afterEdit.sha256, "same-size content edit escaped SHA-256 identity")
    print("same_size_content_edit=detected_by_sha256")

    // Symlink negative case: inspect the directory entry without following it;
    // reject it from the regular-file path. Both target and link are fixtures.
    let linkTarget = root.appendingPathComponent("symlink-target.txt")
    let link = root.appendingPathComponent("symlink-fixture")
    try writeFixture("symlink-target-fixture", to: linkTarget)
    try fm.createSymbolicLink(at: link, withDestinationURL: linkTarget)
    let linkValues = try link.resourceValues(forKeys: [.isSymbolicLinkKey])
    try require(linkValues.isSymbolicLink == true, "symlink was not identified without following it")
    var symlinkRejected = false
    do { _ = try identity(of: link) } catch ProbeError.identityUnavailable { symlinkRejected = true }
    try require(symlinkRejected, "regular-file identity accepted a symlink")
    print("symlink_negative=identified_and_rejected")

    if selectivePurge {
        try runSelectivePurge(root: root, fileManager: fm)
        return
    }

    // Native Trash test: only this newly created file is supplied to Trash.
    // Never pass user input, a directory, the symlink, or any enumerated Trash
    // path. The returned URL is the sole authority for the restore attempt.
    let trashFixture = root.appendingPathComponent("native-trash-fixture.txt")
    try writeFixture("native-trash-self-created-fixture", to: trashFixture)
    let trashID = try identity(of: trashFixture)
    let returnedURL: URL
    do {
        returnedURL = try stageSelfCreatedFile(trashFixture, expected: trashID, fileManager: fm)
    } catch {
        print("native_trash=unsupported (Trash stage or receipt validation failed: \(error))")
        print("selective_permanent_removal=unverified (optional purge mode not run; no delete attempted)")
        print("limitations=path-based Trash has a residual race; native restore, crash reconciliation, permissions matrix, other-volume behavior, purge, and free-space recovery remain unverified")
        return
    }
    print("native_trash=staged")
    print("native_trash_returned_url=\(returnedURL.path)")
    print("native_trash_receipt_identity_matches=\((try? identity(of: returnedURL)) == trashID)")

    // Do not guess another Trash path or delete anything. Restore only from the
    // exact returnedURL if its resource identity still matches and the original
    // path is unoccupied. Collision leaves the native Trash item untouched.
    guard (try? identity(of: returnedURL)) == trashID else {
        print("native_trash_restore=unsupported (returnedURL identity did not match; left untouched)")
        print("selective_permanent_removal=unverified (optional purge mode not run; no delete attempted)")
        return
    }
    guard !fm.fileExists(atPath: trashFixture.path) else {
        print("native_trash_restore=blocked (original path occupied; left returnedURL untouched)")
        print("selective_permanent_removal=unverified (optional purge mode not run; no delete attempted)")
        return
    }
    do {
        try fm.moveItem(at: returnedURL, to: trashFixture)
        try require(try identity(of: trashFixture) == trashID, "native returnedURL restore identity mismatch")
        print("native_trash_restore=passed (moved exact returnedURL after collision and identity checks)")
    } catch {
        print("native_trash_restore=unsupported (restore failed; no delete attempted: \(error))")
    }
    print("selective_permanent_removal=unsupported (Foundation provides no exact-item purge proof here; not attempted)")
    print("limitations=path-based Trash staging has a residual race; no crash/restart reconciliation, permissions matrix, other-volume test, purge test, or free-space recovery test")
}

do {
    let arguments = Array(CommandLine.arguments.dropFirst())
    guard arguments.isEmpty || arguments == ["--probe-selective-purge"] else {
        throw ProbeError.assertion("accepted invocation is no arguments or --probe-selective-purge")
    }
    try run(arguments == ["--probe-selective-purge"])
} catch {
    fputs("probe_failed=\(error)\n", stderr)
    exit(EXIT_FAILURE)
}
