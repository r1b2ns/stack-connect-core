//! Dev-only probe: runs the real Apple ID web sign-in from a terminal and
//! checks what the session reaches, so a change on Apple's side can be
//! diagnosed without the app.
//!
//! ```text
//! P='cargo run -q -p stack_core --features apple-signin-probe --bin apple_signin_probe --'
//! $P sign-in you@example.com --session-file ~/.stackconnect-session.json
//! $P check        --session-file ~/.stackconnect-session.json
//! $P provisioning --session-file ~/.stackconnect-session.json [--portal-team ABCDE12345]
//! $P client       --session-file ~/.stackconnect-session.json [--team-id 118291181]
//! $P provisioning-client --session-file ~/.stackconnect-session.json [--team-id 118291181]
//! $P beta-groups  --session-file ~/.stackconnect-session.json --app <app id> [--team-id 118291181]
//! $P write-test   --session-file ~/.stackconnect-session.json --app <app id> --group <beta group id>
//! ```
//!
//! - `sign-in`: the password is read from the terminal without echo and is
//!   never stored; then the two-factor code (or `sms <phone id>`). Lists the
//!   teams, switches team (`--team-id`, `APPLE_TEAM_ID`, or a prompt when
//!   there are several), calls `GET iris/v1/apps?limit=1`, and writes the
//!   session blob to `--session-file` (mode 0600) when given. The Apple ID
//!   comes from the argument or `APPLE_ID`. A bare `<apple-id>` argument is
//!   `sign-in` (the Phase 1 usage).
//! - `check`: restores the saved blob, lists the teams and calls iris once.
//! - `provisioning`: read-only verdict table for devices, certificates,
//!   bundle ids and profiles, both on iris and on `developer.apple.com/
//!   services-account/v1` the way spaceship proxies them (a `POST` with
//!   `X-HTTP-Method-Override: GET` and the Developer Portal team id). When
//!   `listTeams.action` fails, its status and Apple's reason are printed and
//!   `--portal-team` still drives the `services-account/v1` probes.
//! - `client`: the real core client in session mode
//!   (`connect_with_apple_session`): `capabilities`, `validate`, `fetch_apps`.
//! - `provisioning-client`: read-only; the real core provisioning reads in
//!   session mode (devices, certificates, bundle ids, profiles on
//!   `services-account`): the count and the first item's id and name of each,
//!   and the Developer Portal team the core resolved and saved. No
//!   provisioning write is offered: devices can only be disabled, not
//!   deleted, and certificates count against the team's limits.
//! - `beta-groups`: read-only; lists the beta groups of `--app` (id, name,
//!   internal or external) through the core client in session mode, to pick
//!   the `--group` of `write-test`.
//! - `write-test`: the one write. Renames the beta group `--group` of
//!   `--app` and renames it back, after a y/N confirmation.
//!
//! Refreshed cookies are written back to `--session-file` by every command
//! that uses the session. Nothing prints a cookie, a token, a header or the
//! password: only statuses, Apple's error codes and titles, counts and names.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::Value;
use stack_core::apple_signin::{
    failure_reason, portal_teams, summarize_errors, AppleCodeDelivery, AppleCodeMethod,
    AppleEndpoints, AppleSession, AppleSignIn, AppleSignInError, AppleSignInStep, AppleTeam,
    AppleTrustedPhone, Password, ProbeHost, ProbeRequest, SessionBlob, SessionProbe,
};
use stack_core::{connect_with_apple_session, SessionStore, StackError};

const IRIS_PROBE: &str = "iris/v1/apps?limit=1";
/// The provisioning resources the app uses.
const PROVISIONING: [&str; 4] = ["devices", "certificates", "bundleIds", "profiles"];
/// The suffix the write test appends to the group name.
const RENAME_SUFFIX: &str = " (probe)";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage: apple_signin_probe <command> [options]
  sign-in [<apple-id>] [--session-file PATH] [--team-id ID]
  check        --session-file PATH
  provisioning --session-file PATH [--portal-team ID]
  client       --session-file PATH [--team-id ID]
  provisioning-client --session-file PATH [--team-id ID]
  beta-groups  --session-file PATH --app APP_ID [--team-id ID]
  write-test   --session-file PATH --app APP_ID --group GROUP_ID [--team-id ID]";

/// Parsed command line.
#[derive(Debug, Default)]
struct Options {
    command: String,
    apple_id: Option<String>,
    session_file: Option<PathBuf>,
    team_id: Option<u64>,
    portal_team: Option<String>,
    app: Option<String>,
    group: Option<String>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut positional: Vec<String> = Vec::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .filter(|v| !v.starts_with("--"))
                .ok_or(format!("{name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--session-file" => options.session_file = Some(expand_home(&value("--session-file")?)),
            "--team-id" => {
                let raw = value("--team-id")?;
                options.team_id = Some(raw.parse().map_err(|_| format!("not a team id: {raw}"))?);
            }
            "--portal-team" => options.portal_team = Some(value("--portal-team")?),
            "--app" => options.app = Some(value("--app")?),
            "--group" => options.group = Some(value("--group")?),
            "-h" | "--help" => return Err(USAGE.to_string()),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"))
            }
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    match positional.next() {
        Some(cmd) if cmd.contains('@') => {
            options.command = "sign-in".into();
            options.apple_id = Some(cmd);
        }
        Some(cmd) => {
            options.command = cmd;
            options.apple_id = positional.next();
        }
        None => return Err(USAGE.to_string()),
    }
    if options.team_id.is_none() {
        options.team_id = std::env::var("APPLE_TEAM_ID")
            .ok()
            .and_then(|t| t.trim().parse().ok());
    }
    Ok(options)
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

async fn run() -> Result<(), String> {
    let options = parse_args(std::env::args().skip(1))?;
    match options.command.as_str() {
        "sign-in" => sign_in(&options).await,
        "check" => check(&options).await,
        "provisioning" => provisioning(&options).await,
        "client" => client(&options).await,
        "provisioning-client" => provisioning_client(&options).await,
        "beta-groups" => beta_groups(&options).await,
        "write-test" => write_test(&options).await,
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn sign_in(options: &Options) -> Result<(), String> {
    let email = options
        .apple_id
        .clone()
        .or_else(|| std::env::var("APPLE_ID").ok())
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .ok_or(format!(
            "sign-in needs an Apple ID (argument or APPLE_ID)\n{USAGE}"
        ))?;

    let password = rpassword::prompt_password("Apple ID password (hidden, not saved): ")
        .map_err(|e| format!("could not read the password: {e}"))?;
    let password = Password::new(password);

    let mut sign_in = AppleSignIn::new(AppleEndpoints::default()).map_err(describe)?;
    println!("Signing in…");
    let mut step = sign_in.start(&email, password).await.map_err(describe)?;

    let teams = loop {
        match step {
            AppleSignInStep::Done { teams } => break teams,
            AppleSignInStep::NeedsAcknowledgement => {
                return Err(describe(AppleSignInError::AcknowledgementRequired));
            }
            AppleSignInStep::NeedsCode {
                ref phones,
                code_length,
                delivery,
                ..
            } => {
                step =
                    two_factor(&mut sign_in, phones, code_length, delivery, step.clone()).await?;
            }
        }
    };

    print_teams(&teams);
    let mut session = sign_in.into_session().map_err(describe)?;
    choose_team(&mut session, &teams, options.team_id).await?;
    iris_check(&session).await?;

    let blob = session.to_blob().map_err(describe)?;
    let restored = AppleSession::restore(&blob, AppleEndpoints::default()).map_err(describe)?;
    let olympus = restored.olympus_session().await.map_err(describe)?;
    println!(
        "session blob: {} bytes; restored and accepted by App Store Connect ({} team(s))",
        blob.as_bytes().len(),
        olympus.teams.len()
    );
    match &options.session_file {
        Some(path) => {
            write_private(path, blob.as_bytes())?;
            println!("saved the session to {} (mode 0600)", path.display());
        }
        None => println!("not saved (pass --session-file PATH to reuse it)"),
    }
    Ok(())
}

async fn check(options: &Options) -> Result<(), String> {
    let (path, session) = restore(options)?;
    let olympus = session.olympus_session().await.map_err(describe)?;
    print_teams(&olympus.teams);
    println!(
        "current team: {}",
        olympus
            .current_team_id
            .map_or_else(|| "none".to_string(), |id| id.to_string())
    );
    iris_check(&session).await?;
    save_back(&path, &session)
}

async fn provisioning(options: &Options) -> Result<(), String> {
    let (path, session) = restore(options)?;
    let mut probe = SessionProbe::new(&session);
    let mut rows: Vec<Row> = Vec::new();

    const TEAMS_ROUTE: &str = "POST services-account/QH65B2/account/listTeams.action";
    let portal = match probe.send(&ProbeRequest::portal_teams()).await {
        Ok(reply) => {
            let portal = portal_teams(&reply.body);
            rows.push(Row::new(
                "portal teams",
                TEAMS_ROUTE,
                reply.status,
                &reply.body,
                Some(portal.len()),
            ));
            if portal.is_empty() {
                println!(
                    "listTeams returned no team: HTTP {} ({}; Content-Type: {}, \
                     Content-Encoding: {})",
                    reply.status,
                    failure_reason(&reply.body),
                    or_none(&reply.content_type),
                    or_none(&reply.content_encoding)
                );
            }
            portal
        }
        Err(err) => {
            println!("listTeams failed: {}", describe(err));
            rows.push(Row::skipped("portal teams", TEAMS_ROUTE));
            Vec::new()
        }
    };
    println!("Developer Portal teams ({}):", portal.len());
    for (id, name, kind) in &portal {
        println!("  {id}  {name}  ({kind})");
    }
    if portal.is_empty() && options.portal_team.is_none() {
        println!("  (pass --portal-team <id> to probe services-account/v1 anyway)");
    }
    let portal_team = options
        .portal_team
        .clone()
        .or_else(|| portal.first().map(|(id, _, _)| id.clone()));

    for resource in PROVISIONING {
        let iris = ProbeRequest {
            host: ProbeHost::AppStoreConnect,
            method: reqwest::Method::GET,
            path: format!("iris/v1/{resource}?limit=1"),
            headers: vec![
                ("Accept".into(), "application/json".into()),
                ("x-csrf-itc".into(), "[asc-ui]".into()),
            ],
            body: None,
        };
        let reply = probe.send(&iris).await.map_err(describe)?;
        rows.push(Row::new(
            resource,
            &format!("GET iris/v1/{resource}?limit=1"),
            reply.status,
            &reply.body,
            data_count(&reply.body),
        ));

        match &portal_team {
            Some(team) => {
                let request = ProbeRequest::provisioning_list(resource, team, "limit=1");
                let reply = probe.send(&request).await.map_err(describe)?;
                rows.push(Row::new(
                    resource,
                    &format!("POST services-account/v1/{resource} (override GET)"),
                    reply.status,
                    &reply.body,
                    data_count(&reply.body),
                ));
            }
            None => rows.push(Row::skipped(
                resource,
                "services-account/v1 (no Developer Portal team)",
            )),
        }
    }

    print_rows(&rows);
    save_back(&path, &session)
}

async fn client(options: &Options) -> Result<(), String> {
    let (path, team_id) = client_target(options)?;
    let store = Arc::new(FileSessionStore { path: path.clone() });
    let provider =
        connect_with_apple_session("probe".into(), team_id, store, None).map_err(describe_stack)?;
    println!("connected for team {team_id}");
    println!("capabilities: {:?}", provider.capabilities());
    match provider.validate().await {
        Ok(()) => println!("validate: ok"),
        Err(err) => println!("validate: {}", describe_stack(err)),
    }
    match provider.fetch_apps().await {
        Ok(apps) => {
            println!("fetch_apps: {} app(s)", apps.len());
            for app in apps.iter().take(20) {
                println!("  {}  {}  {}", app.id, app.bundle_id, app.name);
            }
        }
        Err(err) => println!("fetch_apps: {}", describe_stack(err)),
    }
    Ok(())
}

async fn provisioning_client(options: &Options) -> Result<(), String> {
    let (path, team_id) = client_target(options)?;
    let store = Arc::new(FileSessionStore { path: path.clone() });
    let provider =
        connect_with_apple_session("probe".into(), team_id, store, None).map_err(describe_stack)?;
    println!("connected for team {team_id}");
    let missing = |name: &str| format!("the provider has no {name} capability");

    let devices = provider.devices().ok_or_else(|| missing("devices"))?;
    print_read(
        "devices",
        devices
            .fetch_devices()
            .await
            .map(|all| summary(&all, |d| (d.id.clone(), d.name.clone()))),
    );
    let certificates = provider
        .certificates()
        .ok_or_else(|| missing("certificates"))?;
    print_read(
        "certificates",
        certificates
            .fetch_certificates()
            .await
            .map(|all| summary(&all, |c| (c.id.clone(), c.display_name.clone()))),
    );
    let bundle_ids = provider.bundle_ids().ok_or_else(|| missing("bundle ids"))?;
    print_read(
        "bundle ids",
        bundle_ids
            .fetch_bundle_ids()
            .await
            .map(|all| summary(&all, |b| (b.id.clone(), b.identifier.clone()))),
    );
    let profiles = provider.profiles().ok_or_else(|| missing("profiles"))?;
    print_read(
        "profiles",
        profiles
            .fetch_profiles()
            .await
            .map(|all| summary(&all, |p| (p.id.clone(), p.name.clone()))),
    );

    // The core saved the resolved portal team with the session.
    let saved = read_blob(&path).and_then(|blob| {
        AppleSession::restore(&blob, AppleEndpoints::default()).map_err(describe)
    })?;
    match saved.saved_portal_team(team_id) {
        Some(portal) => println!("saved Developer Portal team: {portal}"),
        None => println!("saved Developer Portal team: none"),
    }
    Ok(())
}

/// The count of `items` and the first one's `(id, name)`.
fn summary<T>(
    items: &[T],
    first: impl Fn(&T) -> (String, String),
) -> (usize, Option<(String, String)>) {
    (items.len(), items.first().map(first))
}

/// One line per provisioning read.
fn print_read(resource: &str, result: Result<(usize, Option<(String, String)>), StackError>) {
    match result {
        Ok((count, Some((id, name)))) => {
            println!("{resource}: {count} (first: {id}  {name})");
        }
        Ok((count, None)) => println!("{resource}: {count}"),
        Err(err) => println!("{resource}: {}", describe_stack(err)),
    }
}

async fn beta_groups(options: &Options) -> Result<(), String> {
    let app = options
        .app
        .clone()
        .ok_or(format!("beta-groups needs --app\n{USAGE}"))?;
    let (path, team_id) = client_target(options)?;
    let store = Arc::new(FileSessionStore { path });
    let provider =
        connect_with_apple_session("probe".into(), team_id, store, None).map_err(describe_stack)?;
    let groups = provider
        .beta_groups()
        .ok_or("the provider has no beta groups capability")?
        .fetch_beta_groups(app.clone(), 200)
        .await
        .map_err(describe_stack)?;
    println!("Beta groups of app {app} ({}):", groups.len());
    for group in &groups {
        println!(
            "  {}  {:<8}  {}",
            group.id,
            group_kind(group.is_internal_group),
            group.name.as_deref().unwrap_or("(no name)")
        );
    }
    Ok(())
}

/// `internal`, `external` or `?` for a beta group's `isInternalGroup`.
fn group_kind(is_internal: Option<bool>) -> &'static str {
    match is_internal {
        Some(true) => "internal",
        Some(false) => "external",
        None => "?",
    }
}

async fn write_test(options: &Options) -> Result<(), String> {
    let app = options
        .app
        .clone()
        .ok_or(format!("write-test needs --app\n{USAGE}"))?;
    let group = options
        .group
        .clone()
        .ok_or(format!("write-test needs --group\n{USAGE}"))?;
    let (path, team_id) = client_target(options)?;
    let store = Arc::new(FileSessionStore { path });
    let provider =
        connect_with_apple_session("probe".into(), team_id, store, None).map_err(describe_stack)?;
    let groups = provider
        .beta_groups()
        .ok_or("the provider has no beta groups capability")?;

    let all = groups
        .fetch_beta_groups(app.clone(), 200)
        .await
        .map_err(describe_stack)?;
    let current = all.iter().find(|g| g.id == group).ok_or(format!(
        "beta group {group} is not one of app {app}'s {} group(s)",
        all.len()
    ))?;
    let original = current
        .name
        .clone()
        .ok_or("the beta group has no name to restore")?;
    let renamed = format!("{original}{RENAME_SUFFIX}");
    println!("before: \"{original}\"");

    let answer = prompt(&format!(
        "Rename beta group {group} to \"{renamed}\" and then back to \"{original}\"? [y/N] "
    ))?;
    if !matches!(answer.as_str(), "y" | "Y" | "yes") {
        println!("nothing changed");
        return Ok(());
    }

    let after = groups
        .update_beta_group(group.clone(), Some(renamed), None, None, None)
        .await
        .map_err(describe_stack)?;
    println!("after rename: \"{}\"", after.name.unwrap_or_default());

    match groups
        .update_beta_group(group.clone(), Some(original.clone()), None, None, None)
        .await
    {
        Ok(restored) => {
            println!("after revert: \"{}\"", restored.name.unwrap_or_default());
            Ok(())
        }
        Err(err) => Err(format!(
            "THE GROUP WAS NOT RENAMED BACK: rename beta group {group} to \"{original}\" \
             by hand ({})",
            describe_stack(err)
        )),
    }
}

// ---------------------------------------------------------------------------
// Sign-in helpers
// ---------------------------------------------------------------------------

/// One round of the two-factor prompt; returns the next step.
async fn two_factor(
    sign_in: &mut AppleSignIn,
    phones: &[AppleTrustedPhone],
    code_length: u8,
    delivery: AppleCodeDelivery,
    current: AppleSignInStep,
) -> Result<AppleSignInStep, String> {
    match delivery {
        AppleCodeDelivery::TrustedDevices => {
            println!("A code was sent to your trusted Apple devices.")
        }
        AppleCodeDelivery::Phone { phone_id } => println!("A code was sent to phone {phone_id}."),
        AppleCodeDelivery::NotSent => {
            println!("No code was sent yet: pick a phone with `sms <id>`.")
        }
    }
    println!("Trusted phones:");
    for phone in phones {
        println!("  [{}] {} ({})", phone.id, phone.masked_number, phone.mode);
    }
    let answer = prompt(&format!(
        "Enter the {code_length}-digit code, or `sms <phone id>` for a text message: "
    ))?;

    if let Some(rest) = answer.strip_prefix("sms") {
        let phone_id = match rest.trim() {
            "" if phones.len() == 1 => phones[0].id,
            "" => return Err("several phones: use `sms <phone id>`".to_string()),
            id => id.parse().map_err(|_| format!("not a phone id: {id}"))?,
        };
        return sign_in.request_sms(phone_id).await.map_err(describe);
    }

    let method = match delivery {
        AppleCodeDelivery::Phone { phone_id } => AppleCodeMethod::Phone { phone_id },
        AppleCodeDelivery::TrustedDevices => AppleCodeMethod::TrustedDevice,
        AppleCodeDelivery::NotSent => {
            println!("Request a text message first.");
            return Ok(current);
        }
    };
    match sign_in.submit_code(&answer, method).await {
        Ok(step) => Ok(step),
        Err(err @ (AppleSignInError::WrongCode | AppleSignInError::InvalidInput { .. })) => {
            println!("{err}; try again.");
            Ok(current)
        }
        Err(err) => Err(describe(err)),
    }
}

fn print_teams(teams: &[AppleTeam]) {
    println!("Teams ({}):", teams.len());
    for team in teams {
        println!("  {}  {}", team.provider_id, team.name);
    }
}

/// `wanted`, or a prompt when there are several teams; switches only when the
/// choice differs from the session's current team.
async fn choose_team(
    session: &mut AppleSession,
    teams: &[AppleTeam],
    wanted: Option<u64>,
) -> Result<(), String> {
    let current = session.team_id();
    let wanted = match wanted {
        Some(id) => Some(id),
        None if teams.len() > 1 => {
            let answer = prompt(&format!(
                "Team id to use [{}]: ",
                current.map_or_else(|| "current".to_string(), |id| id.to_string())
            ))?;
            if answer.is_empty() {
                None
            } else {
                Some(
                    answer
                        .parse()
                        .map_err(|_| format!("not a team id: {answer}"))?,
                )
            }
        }
        None => None,
    };
    if let Some(team_id) = wanted.filter(|id| Some(*id) != current) {
        session.select_team(team_id).await.map_err(describe)?;
        println!("Switched to team {team_id}.");
    }
    Ok(())
}

async fn iris_check(session: &AppleSession) -> Result<(), String> {
    let response = session
        .app_store_connect_get(IRIS_PROBE)
        .await
        .map_err(describe)?;
    println!("GET {IRIS_PROBE} → HTTP {}", response.status);
    match serde_json::from_str::<Value>(&response.body) {
        Ok(body) => {
            let count = body
                .get("data")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let total = body
                .pointer("/meta/paging/total")
                .and_then(Value::as_u64)
                .map(|t| format!(" (total {t})"))
                .unwrap_or_default();
            println!("apps in this page: {count}{total}");
        }
        Err(_) => println!("the response is not JSON"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Session file
// ---------------------------------------------------------------------------

fn session_path(options: &Options) -> Result<PathBuf, String> {
    options
        .session_file
        .clone()
        .ok_or(format!("{} needs --session-file\n{USAGE}", options.command))
}

fn read_blob(path: &Path) -> Result<SessionBlob, String> {
    fs::read(path)
        .map(SessionBlob::from_bytes)
        .map_err(|e| format!("cannot read {}: {e} (run `sign-in` first)", path.display()))
}

fn restore(options: &Options) -> Result<(PathBuf, AppleSession), String> {
    let path = session_path(options)?;
    let session =
        AppleSession::restore(&read_blob(&path)?, AppleEndpoints::default()).map_err(describe)?;
    Ok((path, session))
}

/// The session file and the team the core client acts for: `--team-id` /
/// `APPLE_TEAM_ID`, else the team the saved session last selected.
fn client_target(options: &Options) -> Result<(PathBuf, u64), String> {
    let (path, session) = restore(options)?;
    let team_id = options
        .team_id
        .or_else(|| session.team_id())
        .ok_or("no team: pass --team-id (see `check` for the ids)")?;
    Ok((path, team_id))
}

fn save_back(path: &Path, session: &AppleSession) -> Result<(), String> {
    let blob = session.to_blob().map_err(describe)?;
    write_private(path, blob.as_bytes())
}

/// Writes `bytes` to `path` readable by the owner only (0600), through a
/// temporary file renamed over it.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut open = fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut file = open
        .open(&tmp)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot restrict {}: {e}", tmp.display()))?;
    }
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The core's [`SessionStore`] over the session file (one account).
struct FileSessionStore {
    path: PathBuf,
}

impl SessionStore for FileSessionStore {
    fn load(&self, _account_id: String) -> Option<Vec<u8>> {
        fs::read(&self.path).ok()
    }

    fn save(&self, _account_id: String, blob: Vec<u8>) {
        if let Err(message) = write_private(&self.path, &blob) {
            eprintln!("warning: could not save the refreshed session: {message}");
        }
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// One line of the provisioning verdict table.
struct Row {
    resource: String,
    route: String,
    status: Option<u16>,
    count: Option<usize>,
    errors: String,
}

impl Row {
    fn new(resource: &str, route: &str, status: u16, body: &str, count: Option<usize>) -> Self {
        Self {
            resource: resource.to_string(),
            route: route.to_string(),
            status: Some(status),
            count,
            errors: summarize_errors(body),
        }
    }

    fn skipped(resource: &str, route: &str) -> Self {
        Self {
            resource: resource.to_string(),
            route: route.to_string(),
            status: None,
            count: None,
            errors: String::new(),
        }
    }

    fn verdict(&self) -> &'static str {
        match self.status {
            None => "skipped",
            Some(s) if (200..300).contains(&s) => "reachable",
            Some(401 | 403) => "refused",
            Some(404) => "missing",
            Some(_) => "failed",
        }
    }
}

fn print_rows(rows: &[Row]) {
    println!();
    println!(
        "{:<13} {:<56} {:>6} {:>5}  {:<9}  apple errors",
        "resource", "route", "status", "items", "verdict"
    );
    for row in rows {
        println!(
            "{:<13} {:<56} {:>6} {:>5}  {:<9}  {}",
            row.resource,
            row.route,
            row.status
                .map_or_else(|| "-".to_string(), |s| s.to_string()),
            row.count.map_or_else(|| "-".to_string(), |c| c.to_string()),
            row.verdict(),
            row.errors
        );
    }
}

/// `value`, or `none` when empty.
fn or_none(value: &str) -> &str {
    if value.is_empty() {
        "none"
    } else {
        value
    }
}

fn data_count(body: &str) -> Option<usize> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::len)
}

fn prompt(question: &str) -> Result<String, String> {
    print!("{question}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the answer: {e}"))?;
    Ok(line.trim().to_string())
}

/// The error's message (never a secret: see `AppleSignInError`).
fn describe(err: AppleSignInError) -> String {
    err.to_string()
}

/// A core error, cut so an HTML or long JSON body does not flood the terminal.
fn describe_stack(err: StackError) -> String {
    let text = err.to_string();
    let cut: String = text.chars().take(400).collect();
    if cut.len() < text.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_commands_and_the_phase_1_form() {
        let o = parse_args(args(&["dev@example.com"])).unwrap();
        assert_eq!(o.command, "sign-in");
        assert_eq!(o.apple_id.as_deref(), Some("dev@example.com"));

        let o = parse_args(args(&[
            "write-test",
            "--session-file",
            "/tmp/s.json",
            "--app",
            "1",
            "--group",
            "g",
            "--team-id",
            "42",
        ]))
        .unwrap();
        assert_eq!(o.command, "write-test");
        assert_eq!(o.session_file, Some(PathBuf::from("/tmp/s.json")));
        assert_eq!(
            (o.app.as_deref(), o.group.as_deref()),
            (Some("1"), Some("g"))
        );
        assert_eq!(o.team_id, Some(42));

        let o = parse_args(args(&[
            "beta-groups",
            "--session-file",
            "/tmp/s.json",
            "--app",
            "123",
        ]))
        .unwrap();
        assert_eq!(o.command, "beta-groups");
        assert_eq!(o.app.as_deref(), Some("123"));

        let o = parse_args(args(&[
            "provisioning-client",
            "--session-file",
            "/tmp/s.json",
            "--team-id",
            "7",
        ]))
        .unwrap();
        assert_eq!(o.command, "provisioning-client");
        assert_eq!(o.team_id, Some(7));

        let o = parse_args(args(&["provisioning", "--portal-team", "ABCDE12345"])).unwrap();
        assert_eq!(o.portal_team.as_deref(), Some("ABCDE12345"));

        assert!(parse_args(args(&[])).is_err());
        assert!(parse_args(args(&["check", "--session-file"])).is_err());
        assert!(parse_args(args(&["check", "--team-id", "x"])).is_err());
        assert!(parse_args(args(&["check", "--bogus"])).is_err());
    }

    #[test]
    fn group_kinds() {
        assert_eq!(group_kind(Some(true)), "internal");
        assert_eq!(group_kind(Some(false)), "external");
        assert_eq!(group_kind(None), "?");
        assert_eq!(or_none(""), "none");
        assert_eq!(or_none("gzip"), "gzip");
    }

    #[cfg(unix)]
    #[test]
    fn session_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("probe-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let store = FileSessionStore { path: path.clone() };
        store.save("a".into(), b"three".to_vec());
        assert_eq!(store.load("a".into()), Some(b"three".to_vec()));
        fs::remove_dir_all(&dir).unwrap();
    }
}
