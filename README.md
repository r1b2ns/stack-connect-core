# stack-connect-core

Shared Rust core for [stack-connect](../stack-connect), consumed natively by iOS
via UniFFI (`StackCore.xcframework`). See [RUST_CORE_PLAN.md](RUST_CORE_PLAN.md)
for the full plan and roadmap.

## Status

**Arquitetura multi-serviço por plugins.** O core é um hub: cada serviço externo
(App Store Connect, Firebase, Google hoje; AWS, GitHub, … no futuro) entra como um
`Provider` que implementa um contrato comum, registrado num `registry`. Adicionar
um serviço não toca o núcleo nem o facade UniFFI — ver [RUST_CORE_PLAN.md](RUST_CORE_PLAN.md)
(§3 contrato, §4 como adicionar um serviço).

**Phase 0 — esqueleto + prova de binding ✅.** Workspace Cargo, facade UniFFI,
callback `CredentialStore`, `StackError`, xcframework (iOS device/sim) + smoke/XCTest.

**Phase 1 — contrato de serviço + 1º plugin (App Store Connect) ✅.** `service::{provider,
kind, registry}` (`Provider`/`ServiceKind`/`Capability`/`CredentialField`), `auth::es256`
(`.p8`/P-256), `providers/appstore` (`validate` + `fetch_apps` via `GET /v1/apps`,
paginação `links.next`), facade `connect()`/`credential_schema()`/`available_services()`.
Sample Google removido. **17 testes Rust** + host smoke + XCTest no simulador iOS — verdes.

Próximo: **Fase 2** — plugar o App Store Connect no app iOS (strangler) + capacidades
ASC completas (~31 recursos) + `SyncService` sobre `BlobStore`.

## Layout

```
crates/stack_core/      # o crate do core
  src/
    api/play.rs         # cliente Play Developer Reporting (apps:search)
    auth/               # service account · JWT RS256 · OAuth2 + cache
    domain.rs           # AppInfo (uniffi::Record)
    error.rs            # StackError (uniffi::Error)
    ports.rs            # CredentialStore (foreign trait)
    facade.rs           # PlayProvider (uniffi::Object)
    bin/uniffi-bindgen.rs
  tests/                # smoke (API pública) + fixtures de chave RSA
bindings/swift/         # pacote SwiftPM consumível pelo app (binaryTarget)
  Package.swift         # StackCore.xcframework + StackCore.swift gerado
  smoke/main.swift      # smoke de host (cross-FFI)
  Tests/                # XCTest (iOS simulator)
build/
  gen-swift.sh          # gera os bindings Swift (UniFFI library mode)
  build-xcframework.sh  # gera StackCore.xcframework (iOS device + sim)
  swift-smoke.sh        # compila + roda o smoke cross-FFI no host
```

## Desenvolvimento

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test

# Bindings Swift + xcframework (macOS):
./build/build-xcframework.sh

# Smoke cross-FFI no host (Swift → Rust → callback → erro):
./build/swift-smoke.sh
```

UniFFI é proc-macro puro (`uniffi::setup_scaffolding!()`); o `uniffi-bindgen` é
embutido como bin do crate, fixado na mesma versão do runtime.
