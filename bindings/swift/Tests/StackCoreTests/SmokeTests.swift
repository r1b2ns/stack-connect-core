import XCTest

@testable import StackCoreRust

// Cross-FFI smoke for `swift test` / xcodebuild on an iOS simulator (the xcframework
// carries iOS slices). The host equivalent lives in ../../smoke/main.swift.
//
// Offline by design: it only exercises the synchronous metadata surface and the
// synchronous `connect`, which reads secrets via the Swift CredentialStore callback.
final class SmokeTests: XCTestCase {
    func testAppStoreConnectIsAvailableAcrossFFI() {
        XCTAssertTrue(availableServices().contains(.appStoreConnect))
    }

    func testCredentialSchemaCrossesFFI() {
        let keys = credentialSchema(kind: .appStoreConnect).map { $0.key }
        XCTAssertEqual(keys, ["issuerId", "keyId", "privateKeyP8"])
    }

    func testRustCallsBackIntoSwiftCredentialStore() {
        // The store may be queried from any thread by the Rust callback, so guard
        // the recorded queries behind a lock and mark the type @unchecked Sendable.
        final class RecordingStore: CredentialStore, @unchecked Sendable {
            private let lock = NSLock()
            private var queries: [(accountId: String, key: String)] = []

            var asked: [(accountId: String, key: String)] {
                lock.lock()
                defer { lock.unlock() }
                return queries
            }

            func secret(accountId: String, key: String) -> String? {
                lock.lock()
                queries.append((accountId: accountId, key: key))
                lock.unlock()
                return nil // force the "missing credentials" path
            }

            func setSecret(accountId: String, key: String, value: String) {}
            func delete(accountId: String) {}
        }

        let store = RecordingStore()
        XCTAssertThrowsError(try connect(kind: .appStoreConnect, accountId: "acct-1", store: store)) { error in
            guard case StackError.InvalidCredentials = error else {
                return XCTFail("expected .InvalidCredentials, got \(error)")
            }
        }

        let first = store.asked.first
        XCTAssertEqual(first?.accountId, "acct-1")
        XCTAssertEqual(first?.key, "issuerId")
    }
}
