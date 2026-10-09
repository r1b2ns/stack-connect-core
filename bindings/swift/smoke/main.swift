import Foundation

// Cross-FFI host smoke test: drive the generated Swift binding into the Rust core.
// Run via ./build/swift-smoke.sh (links the macOS staticlib).
//
// Everything here is OFFLINE: it exercises the synchronous metadata surface
// (availableServices / credentialSchema), the synchronous `connect`, which reads
// secrets through the Swift CredentialStore callback, and async capability calls
// that the core rejects before sending anything — no network is touched.

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("FAIL: \(msg)\n".utf8))
    exit(1)
}

// 1) Service registry crosses the FFI: App Store Connect and Google Play are available.
let services = availableServices()
guard services.contains(.appStoreConnect) else {
    fail("availableServices() did not contain .appStoreConnect: \(services)")
}
print("ok: availableServices() contains .appStoreConnect")
guard services.contains(.googlePlay) else {
    fail("availableServices() did not contain .googlePlay: \(services)")
}
print("ok: availableServices() contains .googlePlay")

// 2) Credential schema crosses the FFI with the expected per-service fields.
let schema = credentialSchema(kind: .appStoreConnect)
let keys = schema.map { $0.key }
guard keys == ["issuerId", "keyId", "privateKeyP8"] else {
    fail("credentialSchema keys mismatch, got: \(keys)")
}
print("ok: credentialSchema(.appStoreConnect) keys are \(keys)")

let playKeys = credentialSchema(kind: .googlePlay).map { $0.key }
guard playKeys == ["clientEmail", "privateKeyId", "privateKey"] else {
    fail("credentialSchema(.googlePlay) keys mismatch, got: \(playKeys)")
}
print("ok: credentialSchema(.googlePlay) keys are \(playKeys)")

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
    _ = try connect(kind: .appStoreConnect, accountId: "acct-1", store: store, debugLogger: nil)
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

// 4) Google Play connects offline (the RSA test key is parsed and test-signed
//    locally) and advertises its capabilities, each reachable as a handle.
final class PlayStore: CredentialStore, @unchecked Sendable {
    let secrets: [String: String]
    init(secrets: [String: String]) { self.secrets = secrets }
    func secret(accountId: String, key: String) -> String? { secrets[key] }
    func setSecret(accountId: String, key: String, value: String) {}
    func delete(accountId: String) {}
}

// swift-smoke.sh runs from the repo root.
let pemPath = "crates/stack_core/tests/fixtures/test_rsa_private_pkcs8.pem"
guard let pem = try? String(contentsOfFile: pemPath, encoding: .utf8) else {
    fail("could not read the test key at \(pemPath)")
}
let playStore = PlayStore(secrets: [
    "clientEmail": "play-bot@my-project.iam.gserviceaccount.com",
    "privateKeyId": "0123456789abcdef0123456789abcdef01234567",
    "privateKey": pem,
])

let play: Provider
do {
    play = try connect(kind: .googlePlay, accountId: "play-bot", store: playStore, debugLogger: nil)
} catch {
    fail("connect(.googlePlay) failed: \(error)")
}
let playCapabilities = play.capabilities()
guard playCapabilities == [.apps, .reviews, .appDetails, .storeListings, .tracks, .appIcons] else {
    fail("Google Play capabilities mismatch, got: \(playCapabilities)")
}
print("ok: Google Play capabilities are \(playCapabilities)")

guard let playReviews = play.reviews(), let appDetails = play.appDetails(),
      let appIcons = play.appIcons(),
      play.storeListings() != nil, play.tracks() != nil else {
    fail("a Google Play capability handle is missing")
}
guard play.appStoreVersions() == nil, play.builds() == nil else {
    fail("Google Play exposed an App Store Connect-only handle")
}
print("ok: Google Play exposes reviews/appDetails/storeListings/tracks/appIcons handles")

// 5) The new async surface crosses the FFI and its typed errors come back,
//    still offline: each call is rejected locally before any request.
do {
    _ = try await appDetails.fetchAppDetails(appId: "not a package")
    fail("expected fetchAppDetails to reject an invalid package name")
} catch let error as StackError {
    guard case .Http(let status, _) = error, status == 404 else {
        fail("expected .Http(404) for an invalid package name, got \(error)")
    }
    print("ok: fetchAppDetails(invalid package) -> .Http(404) without a request")
} catch {
    fail("unexpected error type: \(error)")
}

do {
    // A name no Play app can have has no store page: nil, decided locally.
    guard try await appIcons.fetchIconUrl(appId: "not a package") == nil else {
        fail("expected fetchIconUrl to answer nil for an invalid package name")
    }
    print("ok: fetchIconUrl(invalid package) -> nil without a request")
} catch {
    fail("fetchIconUrl threw for an invalid package name: \(error)")
}

do {
    _ = try await playReviews.replyToReview(reviewId: "not-a-play-review-id", body: "hi")
    fail("expected replyToReview to reject a foreign review id")
} catch let error as StackError {
    guard case .Decode = error else {
        fail("expected .Decode for a foreign review id, got \(error)")
    }
    print("ok: replyToReview(foreign id) -> .Decode without a request")
} catch {
    fail("unexpected error type: \(error)")
}

do {
    _ = try await playReviews.fetchCustomerReviewsPage(
        appId: "com.example.app", sort: "rating", filterRating: [], limit: 10, pageToken: nil)
    fail("expected fetchCustomerReviewsPage to reject a server-side sort")
} catch let error as StackError {
    guard case .Unsupported = error else {
        fail("expected .Unsupported for sort=rating, got \(error)")
    }
    print("ok: fetchCustomerReviewsPage(sort: rating) -> .Unsupported without a request")
} catch {
    fail("unexpected error type: \(error)")
}

print("SMOKE PASSED")
