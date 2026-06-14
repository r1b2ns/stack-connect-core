import Foundation

// Cross-FFI host smoke test: drive the generated Swift binding into the Rust core.
// Run via ./build/swift-smoke.sh (links the macOS staticlib).
//
// Everything here is OFFLINE: it only exercises the synchronous metadata surface
// (availableServices / credentialSchema) and the synchronous `connect`, which reads
// secrets through the Swift CredentialStore callback — no network is touched.

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("FAIL: \(msg)\n".utf8))
    exit(1)
}

// 1) Service registry crosses the FFI: App Store Connect is available.
let services = availableServices()
guard services.contains(.appStoreConnect) else {
    fail("availableServices() did not contain .appStoreConnect: \(services)")
}
print("ok: availableServices() contains .appStoreConnect")

// 2) Credential schema crosses the FFI with the expected App Store Connect fields.
let schema = credentialSchema(kind: .appStoreConnect)
let keys = schema.map { $0.key }
guard keys == ["issuerId", "keyId", "privateKeyP8"] else {
    fail("credentialSchema keys mismatch, got: \(keys)")
}
print("ok: credentialSchema(.appStoreConnect) keys are \(keys)")

// 3) Foreign-trait callback: while connecting, Rust calls back into the Swift
//    CredentialStore. With no secrets stored, `connect` must throw
//    .InvalidCredentials, and the store must have been asked FIRST for the
//    `issuerId` of "acct-1" — proving the Rust→Swift callback crossed the FFI.
final class RecordingStore: CredentialStore, @unchecked Sendable {
    var asked: [(accountId: String, key: String)] = []
    func secret(accountId: String, key: String) -> String? {
        asked.append((accountId: accountId, key: key))
        return nil // force the "missing credentials" path
    }
    func setSecret(accountId: String, key: String, value: String) {}
    func delete(accountId: String) {}
}

let store = RecordingStore()
do {
    _ = try connect(kind: .appStoreConnect, accountId: "acct-1", store: store)
    fail("expected connect to throw on missing secret")
} catch let error as StackError {
    guard case .InvalidCredentials = error else {
        fail("expected .InvalidCredentials, got \(error)")
    }
    guard let first = store.asked.first else {
        fail("Rust never called back into the Swift store")
    }
    guard first.accountId == "acct-1", first.key == "issuerId" else {
        fail("Rust did not consult the store first for (acct-1, issuerId), got: \(store.asked)")
    }
    print("ok: Rust invoked the Swift CredentialStore callback first with \(first)")
} catch {
    fail("unexpected error type: \(error)")
}

print("SMOKE PASSED")
