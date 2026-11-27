# Security Policy

stack-connect-core handles credentials that can manage apps and ship builds (App Store Connect API keys, Google service accounts, session cookies), so security reports are taken seriously.

## Reporting a vulnerability

**Please don't open a public issue.** Use GitHub's private reporting instead: open the [Security tab](https://github.com/r1b2ns/stack-connect-core/security) and click **Report a vulnerability**, or go straight to the [private report form](https://github.com/r1b2ns/stack-connect-core/security/advisories/new). Include the affected version or commit and the steps to reproduce.

You can expect a first reply within 7 days. Confirmed issues are fixed in a private fork, released, and disclosed in a security advisory that credits you, unless you'd rather stay anonymous. Issues that affect the iOS app can also be reported in [stack-connect](https://github.com/r1b2ns/stack-connect/security).

## Supported versions

Fixes go into `master` and ship in the next release; only the latest release is supported.

## Scope

In scope, for example:

- Credentials, tokens, cookies or private keys reaching a log sink, the debug logger or an error message unredacted.
- Weaknesses in the `.scexport` account-export crypto (`crypto.rs`).
- Requests sent to hosts other than Apple's and Google's services.
- Memory-safety or FFI issues reachable through the UniFFI or flutter_rust_bridge surface.

Out of scope: issues in Apple's or Google's services themselves, and attacks that need an already compromised device.
