import XCTest

@testable import StackCoreRust

// Cross-FFI smoke for `swift test` / xcodebuild on an iOS simulator (the xcframework
// carries iOS slices). The host equivalent lives in ../../smoke/main.swift.
//
// Offline by design: it exercises the synchronous metadata surface, the
// synchronous `connect`, which reads secrets via the Swift CredentialStore
// callback, and async capability calls the core rejects before any request.
final class SmokeTests: XCTestCase {
    func testAppStoreConnectIsAvailableAcrossFFI() {
        XCTAssertTrue(availableServices().contains(.appStoreConnect))
    }

    func testGooglePlayIsAvailableAcrossFFI() {
        XCTAssertTrue(availableServices().contains(.googlePlay))
    }

    func testCredentialSchemaCrossesFFI() {
        let keys = credentialSchema(kind: .appStoreConnect).map { $0.key }
        XCTAssertEqual(keys, ["issuerId", "keyId", "privateKeyP8"])
    }

    func testGooglePlayCredentialSchemaCrossesFFI() {
        let keys = credentialSchema(kind: .googlePlay).map { $0.key }
        XCTAssertEqual(keys, ["clientEmail", "privateKeyId", "privateKey"])
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
        XCTAssertThrowsError(try connect(kind: .appStoreConnect, accountId: "acct-1", store: store, debugLogger: nil)) { error in
            guard case StackError.InvalidCredentials = error else {
                return XCTFail("expected .InvalidCredentials, got \(error)")
            }
        }

        let first = store.asked.first
        XCTAssertEqual(first?.accountId, "acct-1")
        XCTAssertEqual(first?.key, "issuerId")
    }

    // MARK: - Google Play

    /// The core repo root, found from this file (bindings/swift/Tests/StackCoreTests/).
    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // StackCoreTests
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // swift
        .deletingLastPathComponent() // bindings
        .deletingLastPathComponent() // repo root

    /// Connects Google Play offline with the crate's RSA test key: `connect`
    /// parses and test-signs the key locally, so no network is touched.
    private func connectGooglePlay() throws -> Provider {
        final class PlayStore: CredentialStore, @unchecked Sendable {
            let secrets: [String: String]
            init(secrets: [String: String]) { self.secrets = secrets }
            func secret(accountId: String, key: String) -> String? { secrets[key] }
            func setSecret(accountId: String, key: String, value: String) {}
            func delete(accountId: String) {}
        }

        let pemURL = Self.repoRoot
            .appendingPathComponent("crates/stack_core/tests/fixtures/test_rsa_private_pkcs8.pem")
        let pem = try String(contentsOf: pemURL, encoding: .utf8)
        let store = PlayStore(secrets: [
            "clientEmail": "play-bot@my-project.iam.gserviceaccount.com",
            "privateKeyId": "0123456789abcdef0123456789abcdef01234567",
            "privateKey": pem,
        ])
        return try connect(kind: .googlePlay, accountId: "play-bot", store: store, debugLogger: nil)
    }

    func testGooglePlayCapabilitiesCrossFFI() throws {
        let play = try connectGooglePlay()
        XCTAssertEqual(play.capabilities(), [.apps, .reviews, .appDetails, .storeListings, .tracks])
        XCTAssertNotNil(play.reviews())
        XCTAssertNotNil(play.appDetails())
        XCTAssertNotNil(play.storeListings())
        XCTAssertNotNil(play.tracks())
        XCTAssertNil(play.appStoreVersions())
        XCTAssertNil(play.builds())
    }

    func testGooglePlayRejectsInvalidPackageBeforeAnyRequest() async throws {
        let details = try XCTUnwrap(try connectGooglePlay().appDetails())
        do {
            _ = try await details.fetchAppDetails(appId: "not a package")
            XCTFail("expected an invalid package name to be rejected")
        } catch StackError.Http(let status, _) {
            XCTAssertEqual(status, 404)
        }
    }

    func testGooglePlayRejectsForeignReviewIdBeforeAnyRequest() async throws {
        let reviews = try XCTUnwrap(try connectGooglePlay().reviews())
        do {
            _ = try await reviews.replyToReview(reviewId: "not-a-play-review-id", body: "hi")
            XCTFail("expected a foreign review id to be rejected")
        } catch StackError.Decode {
            // Expected: the composite id is opaque and this one is not Play's.
        }
    }
}
