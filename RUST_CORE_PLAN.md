# Plano: `stack_core` (Rust) + binding Swift/iOS (UniFFI)

> Core compartilhado em Rust que funciona como um **hub multi-serviço**: cada
> serviço externo (App Store Connect, Firebase, Google Play hoje; AWS, GitHub e
> outros no futuro) entra como um **plugin** (`Provider`) que implementa um
> contrato comum. Consumido nativamente pelo iOS via **UniFFI**
> (`StackCore.xcframework`).
> Companion histórico: `../stack-connect/SHARED_CORE_PLAN.md` e `FLUTTER_PLAN.md`.
>
> **Princípio central:** adicionar um serviço novo = adicionar um módulo `providers/<x>/`
> (+ um autenticador, se necessário) e registrá-lo. **Nada** no núcleo, no facade
> UniFFI ou nos outros providers muda.

## 1. Premissas decididas

| Decisão | Escolha | Consequência no design |
|---|---|---|
| Bindings | **Swift/iOS via UniFFI** | API pública desenhada para UniFFI; núcleo **agnóstico** de binding (facade fina) para abrir Kotlin/FRB depois a baixo custo |
| Arquitetura | **Multi-serviço por plugins** (`Provider` + `registry`) | Surface FFI **estável** independente de quantos serviços existam; novo serviço não toca o core |
| 1º serviço | **App Store Connect** | Primeiro plugin completo; valida o contrato com um serviço real |
| Próximos | **Firebase, Google Play** (portando os packages Swift) → depois **AWS, GitHub, …** | Entram como plugins reusando autenticadores existentes ou novos |
| Apple | Cliente *subset* (~31 famílias de endpoint), não o SDK gigante | Evita portar os 2.411 arquivos gerados do `appstoreconnect-swift-sdk` |
| Persistência | **Nativa por plataforma via callback (`BlobStore`)** | Core *stateless*; iOS continua com SwiftData; migração de menor risco |

## 2. Arquitetura alvo (plugins de serviço)

```
            ┌──────────────── iOS app (nativo) ───────────────┐
            │ SwiftUI views · ViewModels (@Observable)         │
            └───────────────────────┬─────────────────────────┘
                                    │ UniFFI (async/await, callbacks)
                ┌───────────────────┴───────────────────┐
                │  facade: available_services()          │
                │          credential_schema(kind)       │
                │          connect(kind, account, store) │  → Arc<dyn Provider>
                └───────────────────┬───────────────────┘
                                    │
                          ┌─────────┴──────────┐
                          │  service::registry  │  (kind → provider, schema)
                          └─────────┬──────────┘
        ┌──────────────┬───────────┼───────────┬───────────────┐
        ▼              ▼           ▼           ▼               ▼
  providers/appstore  firebase  googleplay   (aws)         (github)   ← plugins
        │              │           │           │               │
        └──────────────┴─────┬─────┴───────────┴───────────────┘
                             │ usam
              ┌──────────────┴──────────────┐
              │ auth (ES256·OAuth-JWT·OAuth2·SigV4)  ·  http (reqwest)
              └──────────────┬──────────────┘
            HTTPS ▼                         ▲ callbacks (UniFFI)
   ┌──────────────────────┐     ┌───────────┴───────────────┐
   │ APIs externas         │     │ CredentialStore→Keychain  │
   │ (Apple/Google/AWS/…)  │     │ BlobStore→SwiftData        │
   └──────────────────────┘     └───────────────────────────┘
```

O facade UniFFI expõe **um contrato uniforme** (`Provider` + factory). Quem conhece
o gerador de binding é só o facade; `domain`/`service`/`providers`/`auth` nunca
importam `uniffi`.

## 3. Contrato de serviço (o coração da extensibilidade)

Tipos exportados pelo UniFFI, estáveis independentemente de quantos serviços existam:

```rust
/// Que serviço externo uma conta conectada fala. Cresce com plugins novos.
#[derive(uniffi::Enum)]
pub enum ServiceKind { AppStoreConnect, Firebase, GooglePlay /* futuros: Aws, GitHub, … */ }

/// Capacidade que um provider PODE expor. A UI chama `capabilities()` p/ saber o que há.
#[derive(uniffi::Enum)]
pub enum Capability { Apps, Builds, Reviews, RemoteConfig, Messaging /* … */ }

/// Um campo de credencial que o serviço exige — dirige o formulário "conectar conta" na UI.
#[derive(uniffi::Record)]
pub struct CredentialField { pub key: String, pub label: String, pub secret: bool, pub multiline: bool }

/// Contrato uniforme que TODO plugin implementa (exportado como interface UniFFI).
#[uniffi::export(async_runtime = "tokio")]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ServiceKind;
    fn capabilities(&self) -> Vec<Capability>;
    async fn validate(&self) -> Result<(), StackError>;
    /// Capacidade comum; retorna `StackError::Unsupported` se o serviço não a tiver.
    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError>;
}
```

Factory + descoberta (também exportados):

```rust
/// Todos os serviços que o core sabe conectar hoje (dirige um seletor na UI).
#[uniffi::export]
pub fn available_services() -> Vec<ServiceKind>;

/// O formulário de credenciais que a UI deve renderizar p/ um serviço.
#[uniffi::export]
pub fn credential_schema(kind: ServiceKind) -> Vec<CredentialField>;

/// Lê os segredos do host (Keychain) e constrói o provider do `kind`.
#[uniffi::export(async_runtime = "tokio")]
pub fn connect(kind: ServiceKind, account_id: String, store: Arc<dyn CredentialStore>)
    -> Result<Arc<dyn Provider>, StackError>;
```

**Decisão de design — capacidades como métodos uniformes** (em vez de tipos
concretos diferentes por serviço cruzando o FFI): a surface Swift é **uma só**
(`Provider`); o que muda entre serviços é *quais* capacidades vêm em
`capabilities()`. Adicionar **serviço** novo = **zero** mudança na API Swift;
adicionar **capacidade** nova = um método a mais no `Provider`. (Alternativa para
APIs muito ricas: sub-objetos por capacidade, ex. `fn reviews(&self) -> Option<Arc<dyn Reviews>>` —
adotar só se a capacidade ficar grande demais para um método único.)

## 4. Como adicionar um serviço novo (ex.: AWS, GitHub)

Sem tocar `domain`, `facade`, `service::provider` ou os outros plugins:

1. **`ServiceKind::Aws`** (uma variante no enum).
2. **Autenticador** em `auth/` se o esquema for novo (ex.: `auth/sigv4.rs` p/ AWS,
   reuso de `auth/oauth2.rs` p/ GitHub). Atrás do trait `Authenticator`.
3. **`providers/aws/`** implementando `Provider` (+ as `Capability` que oferecer).
4. **Registrar** em `service/registry.rs`: `build(kind)`, `credential_schema(kind)`
   e incluir em `available_services()`.

O facade já expõe o novo serviço automaticamente (a UI o vê em `available_services()`
e pede os segredos certos via `credential_schema()`).

## 5. Camadas: o que migra para Rust vs. o que fica nativo

| Origem (Swift) | LOC | Destino |
|---|---|---|
| `appstoreconnect-swift-sdk` (uso real no app) | ~2.854 | → `providers/appstore` (**1º plugin**) + `auth::es256` |
| `Packages/APIProviderFirebase` | ~1.933 | → `providers/firebase` (plugin) + `auth::oauth_jwt` |
| `Packages/APIProviderPlay` | ~1.082 | → `providers/googleplay` (plugin) + `auth::oauth_jwt` |
| `Packages/StackProtocols` (`AccountConnectionProtocol`, `AppInfo`) | 47 | **dissolvido** no core: `AppInfo`→`domain`; `AccountConnectionProtocol`→ o trait `Provider` |
| `StackCore::PersistentStorable` | 31 | → trait `BlobStore` (callback UniFFI) |
| `StackCore::SwiftDataStorable` | 130 | **fica nativo** (implementa o callback) |
| `StackCore::WidgetIconCache` / `Log` / `AppGroup` | ~104 | **fica nativo** (UIKit/WidgetKit/os.Logger) |
| ViewModels / Views / Coordinators / Keychain | — | **fica nativo** |

## 6. Veredito sobre `StackProtocols`

**Não reescrever como crate separado.** `AppInfo` vira `struct` `serde` em `domain/`
(exportada pelo UniFFI). `AccountConnectionProtocol` (`validateCredentials`/`fetchApps`/
`disconnect`) é **generalizado** no trait `Provider` (multi-serviço). As ViewModels
passam a usar o `Provider` gerado.

## 7. Estrutura do workspace Cargo

```
stack-connect-core/
├── Cargo.toml                 # [workspace]
├── rust-toolchain.toml
├── crates/
│   └── stack_core/
│       ├── Cargo.toml         # auth schemes atrás de features (es256, oauth_jwt, oauth2, sigv4)
│       ├── uniffi.toml
│       ├── src/
│       │   ├── lib.rs         # uniffi::setup_scaffolding!; re-exports
│       │   ├── domain/        # AppInfo, ... (tipos de valor compartilhados)
│       │   ├── ports/         # CredentialStore, BlobStore, Clock (callbacks do host)
│       │   ├── error/         # StackError (+ Unsupported)
│       │   ├── http/          # cliente reqwest tipado + estratégias de paginação
│       │   ├── auth/          # autenticadores plugáveis (trait Authenticator):
│       │   │                  #   es256 (Apple) · oauth_jwt (Google SA) · oauth2 (GitHub) · sigv4 (AWS)
│       │   ├── service/
│       │   │   ├── provider.rs   # trait Provider · Capability
│       │   │   ├── kind.rs       # ServiceKind · CredentialField
│       │   │   └── registry.rs   # build(kind) · credential_schema · available_services
│       │   ├── providers/        # um módulo por serviço concreto:
│       │   │   ├── appstore/     #   1º plugin (App Store Connect)
│       │   │   ├── firebase/     #   (fase posterior)
│       │   │   └── googleplay/   #   (fase posterior)   futuros: aws/ github/
│       │   ├── sync/          # SyncService genérico (por provider/capability) sobre BlobStore
│       │   └── facade/        # #[uniffi::export]: connect() · credential_schema() · available_services()
│       └── tests/             # wiremock + fixtures JSON
├── bindings/swift/            # scaffolding gerado + Package.swift do binary
├── build/                     # build-xcframework.sh · gen-swift.sh · swift-smoke.sh
└── .github/workflows/ci.yml
```

## 8. Callback interfaces (núcleo ↔ nativo)

UniFFI 0.31 recomenda **foreign traits** (`with_foreign`, `Arc<dyn Trait>`) no lugar
do antigo `callback_interface`:

```rust
#[uniffi::export(with_foreign)]
pub trait CredentialStore: Send + Sync {   // genérico: vale p/ qualquer serviço
    fn secret(&self, account_id: String, key: String) -> Option<String>;
    fn set_secret(&self, account_id: String, key: String, value: String);
    fn delete(&self, account_id: String);
}

#[uniffi::export(with_foreign)]
pub trait BlobStore: Send + Sync {          // espelha PersistentStorable
    fn save(&self, type_name: String, id: String, json: String);
    fn fetch(&self, type_name: String, id: String) -> Option<String>;
    fn fetch_all(&self, type_name: String) -> Vec<String>;
    fn delete(&self, type_name: String, id: String);
}
```

As **chaves** de credencial são definidas por cada provider via `credential_schema`
(Apple: `issuerId`/`keyId`/`p8`; Google: `serviceAccountJson`; AWS: `accessKeyId`/
`secretAccessKey`/`region`; GitHub: `token`). O `CredentialStore` em si permanece
genérico (chave→valor por conta). iOS: `KeychainStorable`→`CredentialStore`,
`SwiftDataStorable`→`BlobStore`.

## 9. App Store Connect — o *subset* (1º plugin)

`providers/appstore` cobre as ~31 famílias usadas hoje (de `AppleAccountConnection.swift`):
`apps`, `appInfos`/`appInfoLocalizations`, `appStoreVersions` (+localizations/
phasedReleases/releaseRequests), `builds`, `betaGroups`/`betaTesters`/
`betaBuildLocalizations`, `customerReviews`, `reviewSubmissions`, `users`/
`userInvitations`, `bundleIds`/`certificates`/`devices`/`profiles`,
`analyticsReportRequests`/`analyticsReports`. Auth: `auth::es256` (`.p8`/P-256,
`aud=appstoreconnect-v1`, `exp ≤ 20min`). Paginação JSON:API por `links.next`.

## 10. Dependências (crates)

```toml
reqwest      = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
jsonwebtoken = "9"     # ES256 (.p8 Apple) e RS256 (Google SA), conforme o esquema de auth
tokio        = { version = "1", features = ["rt", "rt-multi-thread", "sync", "macros", "time"] }
thiserror    = "2"
uniffi       = { version = "0.31", features = ["tokio"] }  # async → Swift; proc-macro puro
# futuros, atrás de features: aws-sigv4 (AWS), oauth2 (GitHub), ...

[features]                      # cada esquema de auth/serviço só entra no binário se compilado
default      = ["appstore"]
appstore     = []               # ES256
google       = []               # oauth_jwt (Firebase + Google Play)
# aws        = ["dep:aws-sigv4"]
# github     = []               # oauth2
```

Os providers/autenticadores ficam atrás de **cargo features** — o `.xcframework`
só carrega o que está habilitado.

## 11. Roadmap por fases

- **Fase 0 — Esqueleto + prova de binding. ✅ CONCLUÍDA.** Workspace Cargo; facade UniFFI; callback `CredentialStore` (foreign trait); erro tipado `StackError`; `build-xcframework.sh`/`gen-swift.sh`/`swift-smoke.sh`; smoke Swift de host + **XCTest no simulador iOS** atravessando a fronteira. fmt/clippy verdes. Toolchain Rust 1.96. *(Usou um provedor de exemplo descartável; será substituído pelo contrato de serviço.)*
- **Fase 1 — Contrato de serviço + 1º plugin (App Store Connect). ✅ CONCLUÍDA.** `service::{provider, kind, registry}` (`Provider`/`ServiceKind`/`Capability`/`CredentialField`); `auth::es256`; `providers/appstore` com `validate` + `fetch_apps` (`GET /v1/apps`, paginação `links.next`); facade `connect`/`credential_schema`/`available_services`. Sample Google removido. 17 testes Rust + host smoke + XCTest no simulador verdes. *Falta plugar no app iOS (strangler) — início da Fase 2.*
- **Fase 2 — Capacidades ASC completas + sync.** Os ~31 recursos como capacidades; erro 403 *pending agreements*; `SyncService` genérico sobre `BlobStore`. Migrar o resto de `AppleAccountConnection`.
- **Fase 3 — Plugins Firebase e Google Play.** Portar `APIProviderFirebase`/`APIProviderPlay` para `providers/firebase` e `providers/googleplay`, reusando `auth::oauth_jwt`. Trocar esses provedores no app.
- **Fase 4 — Limpeza.** Remover os packages Swift legados + uso do `appstoreconnect-swift-sdk`; manter nativos só `WidgetIconCache`/`Log`/`AppGroup`.
- **Futuro — novos serviços** (AWS via `auth::sigv4`, GitHub via `auth::oauth2`, …): cada um é só um `providers/<x>/` + registro (ver §4). Sem mexer no core nem no facade.

## 12. Fase 0 — definição de pronto ✅

1. `cargo build` + `cargo clippy -D warnings` + `cargo fmt --check` verdes.
2. Facade UniFFI exporta um objeto provedor `async` + a callback `CredentialStore`.
3. `build/build-xcframework.sh` gera `StackCore.xcframework` (iOS device + sim) e
   `build/gen-swift.sh` gera os bindings Swift.
4. Smoke Swift de host + **XCTest no simulador iOS** validam a chamada cruzando a
   fronteira (erro tipado + callback Rust→Swift).
5. CI (`cargo test`/`clippy`/`fmt`) no GitHub Actions.

## 13. Testes & CI

- **Rust (grosso da cobertura):** unit de cada `providers/<x>` com `wiremock` +
  fixtures JSON (URL/método/headers, DTO→domínio, paginação, erros); *golden tests*
  dos autenticadores (ES256, OAuth-JWT); `registry` (kind→provider, schema);
  `sync` com `BlobStore` fake em memória.
- **Contrato:** um teste por provider garantindo `kind()`/`capabilities()`/`validate()`
  e que capacidades ausentes retornam `Unsupported`.
- **Binding (smoke):** XCTest atravessando a fronteira UniFFI (host + simulador).
- **CI:** `cargo fmt --check` + `clippy -D warnings` + `cargo test` (com matrix de
  features dos providers); build do `.xcframework`; (depois) build do app iOS.

## 14. Riscos & mitigação

| Risco | Mitigação |
|---|---|
| Abstração genérica demais (capacidades que não encaixam em todo serviço) | `Capability` opcional + `Unsupported`; sub-objetos por capacidade só quando necessário |
| Estabilidade da surface FFI ao crescer | Serviço novo não muda a API Swift; só capacidade nova adiciona método (raro) |
| `async` Rust → Swift via UniFFI | Validado na Fase 0; `uniffi` feature `tokio` |
| Paridade de JWT (ES256/RS256) | Golden tests contra tokens que os SDKs Swift geram hoje |
| Binário inchar com muitos serviços | Providers/auth atrás de cargo features; `.xcframework` só com o habilitado |
| Fronteira FFI a cada `BlobStore.save` no sync | Aceitável (sync não é hot-loop); lotear writes por entidade |
