import AppKit
import CryptoKit
import Foundation

// Development-only native confirmation boundary probe. No filesystem action
// exists in this program; its only approval output describes a synthetic plan.
struct SyntheticPlan: Encodable {
    let schema: String
    let planID: String
    let revision: Int
    let action: String
    let targetID: String
    let targetLabel: String
    let rationale: String
    let estimatedBytes: Int
    let recovery: String

    func canonicalDigest() throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let bytes = try encoder.encode(self)
        return SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
    }
}

enum ProbeError: Error, CustomStringConvertible {
    case badArguments

    var description: String { "usage: ConfirmationProbe [--run]" }
}

func makeSyntheticPlan() -> SyntheticPlan {
    SyntheticPlan(
        schema: "disk-steward.synthetic-confirmation.v1",
        planID: "fixture-plan-0001",
        revision: 3,
        action: "recoverable-stage",
        targetID: "fixture-item-0001",
        targetLabel: "Disposable compiler cache fixture",
        rationale: "Generated fixture selected in the synthetic review plan",
        estimatedBytes: 4_194_304,
        recovery: "Can be restored; staging alone does not recover space"
    )
}

func showNativeConfirmation(plan: SyntheticPlan, digest: String) -> Bool {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let estimatedMiB = Double(plan.estimatedBytes) / 1_048_576.0

    let alert = NSAlert()
    alert.alertStyle = .warning
    alert.messageText = "Review this synthetic cleanup plan"
    alert.informativeText = """
    Action: \(plan.action)
    Target: \(plan.targetLabel)
    Target ID: \(plan.targetID)
    Rationale: \(plan.rationale)
    Estimated size: \(String(format: "%.1f", estimatedMiB)) MiB
    Effect: \(plan.recovery)
    Plan revision: \(plan.revision)
    Plan digest: \(digest)

    This development probe performs no filesystem mutation.
    """
    alert.addButton(withTitle: "Cancel")
    alert.addButton(withTitle: "Approve synthetic plan")
    alert.buttons.first?.keyEquivalent = "\u{1b}"

    app.activate(ignoringOtherApps: true)
    let response = alert.runModal()
    return response == .alertSecondButtonReturn
}

func run() throws {
    let arguments = Array(CommandLine.arguments.dropFirst())
    guard arguments.isEmpty || arguments == ["--run"] else { throw ProbeError.badArguments }

    let plan = makeSyntheticPlan()
    let digest = try plan.canonicalDigest()
    print("probe=native-confirmation-only")
    print("plan_id=\(plan.planID)")
    print("revision=\(plan.revision)")
    print("target_id=\(plan.targetID)")
    print("action=\(plan.action)")
    print("digest_sha256=\(digest)")
    print("mutation_capability=none")

    guard arguments == ["--run"] else {
        print("mode=dry-run (no dialog shown)")
        return
    }

    let approved = showNativeConfirmation(plan: plan, digest: digest)
    print("mode=native-dialog")
    print("result=\(approved ? "approved_synthetic_plan" : "canceled")")
    print("decision_plan_id=\(plan.planID)")
    print("decision_revision=\(plan.revision)")
    print("decision_plan_digest_sha256=\(digest)")
    print("mutation=none")
}

do {
    try run()
} catch {
    fputs("confirmation_probe_failed=\(error)\n", stderr)
    exit(EXIT_FAILURE)
}
