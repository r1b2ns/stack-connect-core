# Diagnostic logging

The core can tell the host what it is doing: which requests it sent, what the
server answered, how long it took, and why a call failed, including when a call
failed **before** anything was sent. This page covers the sink the host
installs, the levels and targets, what a line looks like, and what is never
logged.

## Two sinks, two purposes

| | `CoreLogger` (this page) | `DebugLogger` |
| --- | --- | --- |
| Installed with | `set_core_logger(logger, min_level)`, once, process-wide | `connect(…, debug_logger)`, per provider |
| Meant for | production builds, support, "what happened?" | a developer at a debugger |
| Content | one redacted line per event, leveled, with a target | runnable cURL + full pretty-printed JSON responses |
| Secrets | **never**: tokens, keys and JWTs removed, e-mails masked | **verbatim** (`Authorization: Bearer …`, the signed JWT assertion) |

They are independent: installing one does not install the other. Keep
`DebugLogger` behind a debug launch flag; `CoreLogger` is safe to leave on.

## API

Rust (`crates/stack_core/src/ports.rs`, `facade.rs`, `logging/mod.rs`):

```rust
pub enum LogLevel { Error, Warn, Info, Debug }

#[uniffi::export(with_foreign)]
pub trait CoreLogger: Send + Sync {
    fn log(&self, level: LogLevel, target: String, message: String);
}

#[uniffi::export]
pub fn set_core_logger(logger: Option<Arc<dyn CoreLogger>>, min_level: LogLevel);
```

Swift (generated):

```swift
public protocol CoreLogger: AnyObject, Sendable {
    func log(level: LogLevel, target: String, message: String)
}
public enum LogLevel { case error, warn, info, debug }
public func setCoreLogger(logger: CoreLogger?, minLevel: LogLevel)
```

- `min_level` is the most verbose level delivered: `Info` delivers `Error`,
  `Warn` and `Info`. Call `set_core_logger` again to change it, or pass `None`
  to remove the sink.
- Without a sink, or for a disabled level, nothing is formatted.
- `log` is called synchronously on whichever thread the core runs on (a tokio
  worker for async calls). Keep it cheap and non-blocking, and do not call back
  into the core from it.
- Every message is a single line, already redacted (see below).

### iOS host (os.Logger)

```swift
import os
import StackCoreRust

final class RustCoreLogger: CoreLogger {
    private static let subsystem = Bundle.main.bundleIdentifier ?? "StackConnect"

    func log(level: LogLevel, target: String, message: String) {
        // One os.Logger per target, so Console.app can filter by category.
        let logger = Logger(subsystem: Self.subsystem, category: "core.\(target)")
        // The core redacts every message before it gets here, so `.public` is
        // safe; without it the unified log shows `<private>` outside Xcode.
        switch level {
        case .error: logger.error("\(message, privacy: .public)")
        case .warn: logger.warning("\(message, privacy: .public)")
        case .info: logger.info("\(message, privacy: .public)")
        case .debug: logger.debug("\(message, privacy: .public)")
        }
    }
}

// At startup, before the first `connect`:
setCoreLogger(logger: RustCoreLogger(), minLevel: .info)   // .debug in debug builds
```

The class has no stored state, so it satisfies `Sendable` as declared.

The Dart (FRB) binding does not expose this sink yet.

## Levels

| Level | When |
| --- | --- |
| `Error` | an HTTP 5xx; a transport failure (no response at all) |
| `Warn` | an HTTP 4xx; a local validation failure (nothing sent); a rejected token exchange; a failed write or edit session; a best-effort cleanup that failed |
| `Info` | writes and their outcome (users, grants, release changes, edit commits, review replies); a 401 retry and its token refresh; totals of list reads |
| `Debug` | one line per HTTP request; edit sessions of reads; page-by-page counts; token minting |

## Targets

| Target | What |
| --- | --- |
| `http` | the shared HTTP choke point: every request of every provider and of the token exchange |
| `auth` | access tokens: Google token exchanges, cache drops, 401 retries; App Store Connect JWT signing |
| `googleplay` | transport-level checks shared by every Play call (package names), `apps:search` |
| `googleplay.users` | developer-account users and grants |
| `googleplay.edits` | edit sessions: insert, commit, delete |
| `googleplay.tracks` | release changes and release summaries |
| `googleplay.reviews` | review paging and replies |
| `googleplay.icons` | icons from the public store page |
| `appstore` | App Store Connect review-submission decisions |

## Line formats

HTTP (`http`): method, host and percent-decoded path, **never the query**,
status and duration. A non-2xx line adds the response's request id when the
server sends one (`x-request-id`, `x-goog-request-id`, `x-apple-request-uuid`,
`x-cloud-trace-context`) and a summary of the error body:

- Google (AIP-193): `google error: code=… status=… message="…" details=[ErrorInfo reason=… domain=…; …] reasons=[…]`
  (`details[].@type` short name with its `reason`/`domain`, legacy
  `errors[].reason`);
- Google OAuth: `oauth error: invalid_grant (…)`;
- App Store Connect: `api errors: [status="…" code="…" title="…" detail="…"; …]`;
- anything else: `body (N bytes): <first 200 characters>` (an HTML page read
  through the capped path is never summarized).

A transport failure names the failure kind and the innermost cause, without the
URL (reqwest's own message embeds the full URL, query included).

Example: a developer ID that is not a number (nothing is sent):

```
Warn  [googleplay.users] local validation failed: developer ID "abc" is not numeric (3 chars: letters); it is the number after /developers/ in a Play Console URL; request not sent to Google
```

Example: Google does not know the developer account:

```
Debug [auth]             Google: requesting an access token for p***@my-project.iam.gserviceaccount.com (none is cached)
Debug [http]             POST oauth2.googleapis.com/token → 200 OK (212 ms)
Debug [auth]             Google: access token obtained, expires in 3599 s
Debug [googleplay.users] users.list developers/5374/users
Warn  [http]             GET androidpublisher.googleapis.com/androidpublisher/v3/developers/5374/users → 404 Not Found (143 ms); google error: code=404 status=NOT_FOUND message="Requested entity was not found."
```

Example: the service account lacks "Admin (all permissions)":

```
Warn  [http]             GET androidpublisher.googleapis.com/androidpublisher/v3/developers/5374/users → 403 Forbidden (121 ms); google error: code=403 status=PERMISSION_DENIED message="The caller does not have permission" details=[ErrorInfo reason=… domain=…]
```

Example: an invitation whose second grant fails, rolled back (abridged):

```
Info  [googleplay.users] users.create developers/5374/users/d***@agency.example: 1 account permissions, 2 grants, no expiry
Info  [googleplay.users] users.create developers/5374/users/d***@agency.example: done
Info  [googleplay.users] grants.create developers/5374/users/d***@agency.example/grants/com.example.ok (1 app permissions): done
Warn  [googleplay.users] grants.create developers/5374/users/d***@agency.example/grants/com.example.gone (0 app permissions): failed: HTTP 400: … (HTTP 400 on POST developers/5374/users/d***@agency.example/grants)
Warn  [googleplay.users] grant for com.example.gone failed after 1 of 2 grants; rolling back the invitation of developers/5374/users/d***@agency.example
Info  [googleplay.users] users.delete developers/5374/users/d***@agency.example: done
```

Example: a release change:

```
Info  [googleplay.tracks] release change on com.example.app track production: SetRollout [42] to 0.5
Info  [googleplay.edits]  edit 0123456789 opened on com.example.app (write)
Info  [googleplay.edits]  committing edit 0123456789 of com.example.app (changesNotSentForReview=true)
Info  [googleplay.tracks] release change on com.example.app track production committed in edit 0123456789; track now: inProgress [42] at 0.5; completed [40]
```

## Errors carry the same context

The `StackError` variants did not change; their messages did.

- **Local validation** (nothing was sent): `Http 404` for a developer ID or
  package name that cannot exist, `Http 400` for an invalid e-mail, grant
  package or release-change value, with the message
  `local validation failed: <what and its shape>; request not sent to Google`.
  The shape is the length and the kinds of characters (`3 chars: letters`,
  `6 chars: digits, whitespace`), so a pasted URL or a trailing space is
  visible without echoing secrets. E-mails are masked in these messages.
- **Google's answer**: Google's message stays first and verbatim (with its
  `[STATUS]`), followed by the HTTP status and the API-relative resource:
  `… Google said: Requested entity was not found. [NOT_FOUND] (HTTP 404 on GET developers/5374/users)`.
  Applies to `Http` and `Auth` errors mapped from a Google response; e-mails in
  the resource path are masked. Google's own message is not altered, so it may
  contain an address Google echoed back.

## Redaction

Call sites only format non-secret facts (method, host, path without query,
status, duration, ids, counts, Google's error fields). Then every message goes
through `logging::redact::scrub` before it reaches the host, as a safety net:

| Removed | Replaced by |
| --- | --- |
| PEM blocks (`-----BEGIN …` to `-----END …-----`), raw or JSON-escaped | `[redacted PEM]` |
| `Bearer <token>` | `Bearer [redacted]` |
| JWTs (`eyJ….….…`) and Google access tokens (`ya29.…`) | `[redacted token]` |
| values of `access_token`, `refresh_token`, `id_token`, `assertion`, `private_key`, `private_key_id`, `client_secret`, `password`, `secret` (`key: value`, `"key": "value"`, `key=value`) | `[redacted]` |
| the local part of every e-mail address | first character + `***` (`d***@agency.example`) |

Never logged at all: request headers, request bodies (the token exchange's
signed assertion included), response bodies of successful calls, review reply
text and release notes (only their length or count), query strings (page
tokens, pre-signed URL signatures).

Allowed in clear: developer account IDs, package names, track names, version
codes, edit ids, review ids, submission ids, HTTP statuses and Google's error
codes, reasons and messages.
