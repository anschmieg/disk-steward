import Foundation
import LocalAuthentication
import Security

/// Development-only Keychain feasibility probe. The live path creates one
/// disposable generic-password item and addresses only its exact identifiers.
struct KeychainProbe {
    static func main() {
        let args = Array(CommandLine.arguments.dropFirst())
        guard args.isEmpty || args == ["--run"] else {
            fputs("Usage: KeychainProbe [--run]\n", stderr)
            exit(2)
        }
        guard args == ["--run"] else {
            print("DRY RUN: no Keychain access performed. Pass --run to test one synthetic item.")
            return
        }
        do {
            try runProbe()
            print("PASS: synthetic Keychain item was read back, deleted, and verified absent.")
        } catch {
            fputs("FAIL: \(error.localizedDescription)\n", stderr)
            exit(1)
        }
    }

    private static func runProbe() throws {
        let service = "com.disk-steward.probe.\(UUID().uuidString)"
        let account = "synthetic-\(UUID().uuidString)"
        let value = Data(UUID().uuidString.utf8)

        var item = exactItemQuery(service: service, account: account)
        item[kSecValueData as String] = value
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

        let addStatus = SecItemAdd(item as CFDictionary, nil)
        guard addStatus == errSecSuccess else {
            throw ProbeError.keychain("add", addStatus)
        }

        var readSucceeded = false
        var primaryError: Error?
        defer {
            // The deletion query is built solely from the two UUID-backed identifiers.
            let deleteStatus = SecItemDelete(exactItemQuery(service: service, account: account) as CFDictionary)
            if deleteStatus != errSecSuccess && deleteStatus != errSecItemNotFound {
                fputs("CLEANUP FAILURE: exact synthetic item deletion returned OSStatus \(deleteStatus).\n", stderr)
            }
        }

        var readQuery = exactItemQuery(service: service, account: account)
        readQuery[kSecReturnData as String] = true
        readQuery[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let readStatus = SecItemCopyMatching(readQuery as CFDictionary, &result)
        guard readStatus == errSecSuccess,
              let readData = result as? Data,
              readData == value else {
            primaryError = ProbeError.verification("read-back", readStatus)
            throw primaryError!
        }
        readSucceeded = true
        _ = readSucceeded // documents that verification precedes the exact deletion below

        let deleteStatus = SecItemDelete(exactItemQuery(service: service, account: account) as CFDictionary)
        guard deleteStatus == errSecSuccess else {
            throw ProbeError.keychain("delete", deleteStatus)
        }
        let absentStatus = SecItemCopyMatching(exactItemQuery(service: service, account: account) as CFDictionary, nil)
        guard absentStatus == errSecItemNotFound else {
            throw ProbeError.verification("absence check", absentStatus)
        }
    }

    private static func exactItemQuery(service: String, account: String) -> [String: Any] {
        let context = LAContext()
        context.interactionNotAllowed = true
        return [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecUseAuthenticationContext as String: context
        ]
    }
}

KeychainProbe.main()

enum ProbeError: LocalizedError {
    case keychain(String, OSStatus)
    case verification(String, OSStatus)

    var errorDescription: String? {
        switch self {
        case let .keychain(operation, status):
            return "Keychain \(operation) failed with OSStatus \(status)."
        case let .verification(step, status):
            return "Synthetic item \(step) failed with OSStatus \(status)."
        }
    }
}
