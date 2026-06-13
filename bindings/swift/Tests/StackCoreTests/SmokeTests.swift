import XCTest

@testable import StackCore

// Cross-FFI smoke for `swift test` / xcodebuild on an iOS simulator (the xcframework
// carries iOS slices). The host equivalent lives in ../../smoke/main.swift.
final class SmokeTests: XCTestCase {
    func testInvalidJsonThrowsAcrossFFI() {
        XCTAssertThrowsError(try PlayProvider(serviceAccountJson: "not valid json")) { error in
            guard case StackError.InvalidCredentials = error else {
                return XCTFail("expected .InvalidCredentials, got \(error)")
            }
        }
    }

    func testRustCallsBackIntoSwiftCredentialStore() {
        final class RecordingStore: CredentialStore, @unchecked Sendable {
            var asked: [String] = []
            func secret(accountId: String, key: String) -> String? {
                asked.append("\(accountId)/\(key)")
                return nil
            }
            func setSecret(accountId: String, key: String, value: String) {}
            func delete(accountId: String) {}
        }

        let store = RecordingStore()
        XCTAssertThrowsError(try PlayProvider.withCredentials(store: store, accountId: "acct-1")) { error in
            guard case StackError.InvalidCredentials = error else {
                return XCTFail("expected .InvalidCredentials, got \(error)")
            }
        }
        XCTAssertEqual(store.asked, ["acct-1/serviceAccountJson"])
    }
}
