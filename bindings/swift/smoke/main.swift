import Foundation

// Cross-FFI host smoke test: drive the generated Swift binding into the Rust core.
// Run via ./build/swift-smoke.sh (links the macOS staticlib).

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("FAIL: \(msg)\n".utf8))
    exit(1)
}

// 1) Error bridging: invalid service-account JSON throws StackError.invalidCredentials.
do {
    _ = try PlayProvider(serviceAccountJson: "not valid json")
    fail("expected PlayProvider init to throw")
} catch let error as StackError {
    guard case .InvalidCredentials = error else {
        fail("expected .InvalidCredentials, got \(error)")
    }
    print("ok: init threw StackError.InvalidCredentials across FFI")
} catch {
    fail("unexpected error type: \(error)")
}

// 2) Foreign-trait callback: Rust calls back into a Swift CredentialStore.
final class RecordingStore: CredentialStore, @unchecked Sendable {
    var asked: [String] = []
    func secret(accountId: String, key: String) -> String? {
        asked.append("\(accountId)/\(key)")
        return nil // force the "missing credentials" path
    }
    func setSecret(accountId: String, key: String, value: String) {}
    func delete(accountId: String) {}
}

let store = RecordingStore()
do {
    _ = try PlayProvider.withCredentials(store: store, accountId: "acct-1")
    fail("expected withCredentials to throw on missing secret")
} catch let error as StackError {
    guard case .InvalidCredentials = error else {
        fail("expected .InvalidCredentials, got \(error)")
    }
    guard store.asked == ["acct-1/serviceAccountJson"] else {
        fail("Rust did not call back into the Swift store as expected: \(store.asked)")
    }
    print("ok: Rust invoked the Swift CredentialStore callback: \(store.asked)")
} catch {
    fail("unexpected error type: \(error)")
}

print("SMOKE PASSED")
