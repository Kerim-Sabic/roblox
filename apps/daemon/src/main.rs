mod legacy_service;
mod quest_scan;

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use nectarpilot_contracts::{Command, CommandEnvelope};
use nectarpilot_core::{
    AutomationEngine, AutomationError, MockBackend, SecretPort, SqliteStore,
    dsl::NectarProgram,
    legacy_ini::import_legacy_ini_files,
    transport::{CommandReceiver, EventSender, NamedPipeSpec},
};
use tokio::sync::broadcast;
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};
use uuid::Uuid;

use legacy_service::LegacyCompatibilityService;
#[cfg(windows)]
use nectarpilot_platform::discover_roblox_clients;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let log_directory = default_data_directory().join("logs");
    if fs::create_dir_all(&log_directory).is_err() {
        eprintln!("NectarPilot could not create its local log directory");
    }
    let file_appender = tracing_appender::rolling::daily(&log_directory, "daemon.jsonl");
    let (file_writer, _log_guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr.and(file_writer))
        .json()
        .init();

    let mut arguments: Vec<String> = env::args().skip(1).collect();
    let command = if arguments
        .first()
        .is_some_and(|value| !value.starts_with('-'))
    {
        arguments.remove(0)
    } else {
        "serve".into()
    };
    let database =
        take_option(&mut arguments, "--database").map_or_else(default_database_path, PathBuf::from);

    match command.as_str() {
        "serve" => serve(&database, &mut arguments).await?,
        "import-ini" => import_ini(&database, &mut arguments)?,
        "export-profile" => export_profile(&database, &mut arguments)?,
        "validate-pattern" => validate_pattern(&arguments)?,
        "doctor" => doctor(&database)?,
        "pipe-spec" => print_pipe_spec(),
        "help" | "--help" | "-h" => print_help(),
        unknown => return Err(format!("unknown daemon command {unknown:?}; use help").into()),
    }
    Ok(())
}

async fn serve(database: &Path, arguments: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    let stdio = take_flag(arguments, "--stdio");
    let pipe = take_flag(arguments, "--pipe");
    let allow_mock_automation = take_flag(arguments, "--mock-automation");
    if !arguments.is_empty() {
        return Err(format!("unknown serve arguments: {}", arguments.join(" ")).into());
    }
    if stdio && pipe {
        return Err("serve accepts only one of --stdio or --pipe".into());
    }

    #[cfg(windows)]
    if !stdio {
        return serve_pipe(database, allow_mock_automation).await;
    }
    #[cfg(not(windows))]
    if pipe {
        return Err("--pipe is available only on Windows".into());
    }
    serve_stdio(database, allow_mock_automation).await
}

fn initialize_engine(
    database: &Path,
    allow_mock_automation: bool,
) -> Result<(Arc<SqliteStore>, AutomationEngine<MockBackend>), Box<dyn Error>> {
    let store = Arc::new(SqliteStore::open(database)?);
    let backend = Arc::new(MockBackend::default());
    if !allow_mock_automation {
        backend.block_normal_mode(
            "native live automation is not parity-ready; use diagnostics/dry-run or the explicitly trusted legacy bridge",
        );
    }
    let engine = AutomationEngine::new(backend, Arc::clone(&store))?;
    match LegacyCompatibilityService::from_environment() {
        Ok(service) => engine.install_legacy_port(Arc::new(service)),
        Err(error) => {
            // Normal native diagnostics remain available. StartLegacy will fail
            // closed until the exact packaged compatibility assets are present.
            tracing::warn!(%error, "legacy compatibility port is unavailable");
        }
    }
    engine.install_secret_port(Arc::new(DpapiSecretPort));
    engine.install_quest_scan_port(Arc::new(quest_scan::QuestScanService::new(
        legacy_service::compatibility_root(),
    )));
    engine.set_report_directory(default_data_directory().join("reports"));

    if store
        .runtime_value("daemon_clean_shutdown")?
        .is_some_and(|value| value == "false")
    {
        engine.record_daemon_crash(Utc::now())?;
    }
    store.set_runtime_value("daemon_clean_shutdown", "false")?;
    Ok((store, engine))
}

/// Seals stored secrets with the current user's Windows DPAPI scope; the
/// plaintext never leaves the daemon process.
struct DpapiSecretPort;

impl SecretPort for DpapiSecretPort {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, AutomationError> {
        nectarpilot_platform::secrets::protect_secret(plaintext)
            .map_err(|error| AutomationError::Backend(error.to_string()))
    }

    fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AutomationError> {
        nectarpilot_platform::secrets::unprotect_secret(ciphertext)
            .map_err(|error| AutomationError::Backend(error.to_string()))
    }
}

/// Passive honey statistics: once a minute, when exactly one restored Roblox
/// client is visible, read the HUD counter and publish a sample. Reading is
/// screen-only; an unconfident read publishes `None` rather than a guess.
#[cfg(windows)]
#[allow(
    clippy::cast_precision_loss,
    reason = "sample timestamps and honey deltas stay far below f64's exact-integer ceiling"
)]
fn spawn_stats_loop(engine: AutomationEngine<MockBackend>) {
    use chrono::Utc;
    use nectarpilot_contracts::StatsSample;
    use nectarpilot_platform::capture::{ClientCapture, WindowsClientCapture};
    use nectarpilot_platform::{HoneyCounterReader, RobloxSession, WindowsOcr};

    std::thread::spawn(move || {
        let ocr = match WindowsOcr::english_us() {
            Ok(ocr) => ocr,
            Err(error) => {
                tracing::info!(%error, "honey statistics disabled: Windows OCR unavailable");
                return;
            }
        };
        let mut reader = HoneyCounterReader::new(ocr);
        let session_start = Utc::now();
        let mut confident: Vec<(chrono::DateTime<Utc>, u64)> = Vec::new();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let honey = discover_roblox_clients()
                .ok()
                .and_then(|clients| {
                    let mut visible = clients.into_iter().filter_map(|client| client.window);
                    match (visible.next(), visible.next()) {
                        (Some(snapshot), None) if !snapshot.geometry.minimized => Some(snapshot),
                        _ => None,
                    }
                })
                .and_then(|snapshot| {
                    WindowsClientCapture
                        .capture(&RobloxSession::from_snapshot(snapshot))
                        .ok()
                })
                .and_then(|frame| reader.read(&frame).actionable(0.0).copied());
            let now = Utc::now();
            if let Some(value) = honey {
                confident.push((now, value));
                // Keep a two-hour window so the rate reflects recent play.
                confident.retain(|(at, _)| now.signed_duration_since(*at).num_minutes() <= 120);
            }
            let honey_per_hour = match confident.as_slice() {
                [first, .., last]
                    if last.0.signed_duration_since(first.0).num_seconds() >= 300
                        && last.1 >= first.1 =>
                {
                    Some(
                        (last.1 - first.1) as f64 * 3600.0
                            / last.0.signed_duration_since(first.0).num_seconds() as f64,
                    )
                }
                _ => None,
            };
            engine.publish_stats(StatsSample {
                sampled_at: now,
                honey,
                honey_per_hour,
                session_minutes: now.signed_duration_since(session_start).num_seconds() as f64
                    / 60.0,
            });
        }
    });
}

#[cfg(not(windows))]
fn spawn_stats_loop(_engine: AutomationEngine<MockBackend>) {}

/// Registers the profile's global control chords and forwards presses as
/// engine commands. Registration failure only disables hotkeys; it never
/// blocks the daemon.
#[cfg(windows)]
fn spawn_hotkey_loop(engine: AutomationEngine<MockBackend>, store: &SqliteStore) {
    use nectarpilot_platform::{WindowsHotkeySet, parse_hotkey};

    let profile_id = engine.snapshot().profile_id;
    let Some(profile) = store.load_profile(profile_id).ok().flatten() else {
        return;
    };
    let hotkeys = profile.automation.hotkeys.clone();
    let session = profile.automation.session.clone();
    let profile_id = profile.id;
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let chords = [
            (1, hotkeys.start.as_str()),
            (2, hotkeys.pause_resume.as_str()),
            (3, hotkeys.stop.as_str()),
            (4, hotkeys.emergency_stop.as_str()),
        ];
        let mut bindings = Vec::new();
        for (id, text) in chords {
            if let Some(chord) = parse_hotkey(text) {
                bindings.push((id, chord.modifiers, chord.virtual_key));
            } else {
                tracing::warn!(hotkey = text, "unparseable hotkey binding skipped");
            }
        }
        // Convenience chords are best-effort. A stale daemon or another app
        // owning F1 must never stop us from attempting Ctrl+Shift+F12, which
        // is the hard input-release route.
        let (mut set, failures) = WindowsHotkeySet::register_best_effort(&bindings);
        for (id, error) in failures {
            let hotkey = chords
                .iter()
                .find_map(|(candidate, hotkey)| (*candidate == id).then_some(*hotkey))
                .unwrap_or("unknown");
            tracing::warn!(
                hotkey,
                emergency_stop = id == 4,
                %error,
                "global hotkey unavailable; remaining controls stay active"
            );
        }
        if set.is_empty() {
            tracing::warn!(
                "no global control hotkeys could be registered; use the desktop controls and resolve the conflict before relying on keyboard controls"
            );
            return;
        }
        tracing::info!("available global control hotkeys registered");
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            for id in set.poll_pressed() {
                let command = match id {
                    1 => Command::StartLegacySession {
                        max_cycles: session.default_max_cycles,
                        max_minutes: session.default_max_minutes,
                    },
                    2 => {
                        if engine.state() == nectarpilot_contracts::RunState::Paused {
                            Command::Resume
                        } else {
                            Command::Pause
                        }
                    }
                    3 => Command::Stop,
                    _ => Command::EmergencyStop,
                };
                let envelope = CommandEnvelope::new(profile_id, command);
                let engine = engine.clone();
                let _ = runtime.block_on(async move { engine.handle_command(envelope).await });
            }
        }
    });
}

#[cfg(not(windows))]
fn spawn_hotkey_loop(_engine: AutomationEngine<MockBackend>, _store: &SqliteStore) {}

async fn serve_stdio(database: &Path, allow_mock_automation: bool) -> Result<(), Box<dyn Error>> {
    let (store, engine) = initialize_engine(database, allow_mock_automation)?;
    spawn_stats_loop(engine.clone());
    spawn_hotkey_loop(engine.clone(), &store);

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut commands = CommandReceiver::new(stdin);
    let mut events = engine.subscribe();
    let mut event_sender = EventSender::new(stdout);

    loop {
        tokio::select! {
            command = commands.next() => match command? {
                Some(command) => {
                    let shutdown_requested = matches!(&command.command, Command::ShutdownDaemon);
                    // Rejections are sent as structured events, so a bad command
                    // does not terminate the daemon transport.
                    let handled = engine.handle_command(command).await;
                    if shutdown_requested && handled.is_ok() {
                        while let Ok(event) = events.try_recv() {
                            event_sender.send(&event).await?;
                        }
                        break;
                    }
                }
                None => break,
            },
            event = events.recv() => match event {
                Ok(event) => event_sender.send(&event).await?,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "stdio client lagged behind daemon events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            signal = tokio::signal::ctrl_c() => {
                signal?;
                break;
            }
        }
    }

    let snapshot = engine.snapshot();
    let _ = engine
        .handle_command(CommandEnvelope::new(
            snapshot.profile_id,
            Command::EmergencyStop,
        ))
        .await;
    store.set_runtime_value("daemon_clean_shutdown", "true")?;
    Ok(())
}

#[cfg(windows)]
async fn serve_pipe(database: &Path, allow_mock_automation: bool) -> Result<(), Box<dyn Error>> {
    use nectarpilot_platform::pipe::SecureNamedPipeListener;

    let spec = NamedPipeSpec::for_current_environment();
    let mut listener = SecureNamedPipeListener::bind(spec.clone())?;
    let (store, engine) = initialize_engine(database, allow_mock_automation)?;
    spawn_stats_loop(engine.clone());
    spawn_hotkey_loop(engine.clone(), &store);
    tracing::info!(path = %spec.path, "secure current-user daemon pipe ready");

    'server: loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                break 'server;
            }
        };
        let (reader, writer) = tokio::io::split(stream);
        let mut commands = CommandReceiver::new(reader);
        let mut events = engine.subscribe();
        let mut event_sender = EventSender::new(writer);

        loop {
            tokio::select! {
                command = commands.next() => match command {
                    Ok(Some(command)) => {
                        let shutdown_requested = matches!(&command.command, Command::ShutdownDaemon);
                        let handled = engine.handle_command(command).await;
                        if shutdown_requested && handled.is_ok() {
                            while let Ok(event) = events.try_recv() {
                                event_sender.send(&event).await?;
                            }
                            break 'server;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "named-pipe client sent an invalid frame");
                        break;
                    }
                },
                event = events.recv() => match event {
                    Ok(event) => {
                        if let Err(error) = event_sender.send(&event).await {
                            tracing::debug!(%error, "named-pipe client disconnected");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "named-pipe client lagged behind daemon events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break 'server,
                },
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    break 'server;
                }
            }
        }
    }

    let snapshot = engine.snapshot();
    let _ = engine
        .handle_command(CommandEnvelope::new(
            snapshot.profile_id,
            Command::EmergencyStop,
        ))
        .await;
    store.set_runtime_value("daemon_clean_shutdown", "true")?;
    Ok(())
}

fn import_ini(database: &Path, arguments: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    let name = take_option(arguments, "--name").unwrap_or_else(|| "Imported Natro profile".into());
    if arguments.is_empty() {
        return Err("import-ini requires one or more INI file paths".into());
    }
    let imported = import_legacy_ini_files(arguments.iter().map(PathBuf::from), name)?;
    let store = SqliteStore::open(database)?;
    store.save_profile(&imported.profile)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "profile_id": imported.profile.id,
            "profile_name": imported.profile.name,
            "mapped_count": imported.report.mapped.len(),
            "unmapped_count": imported.report.unmapped.len(),
            "sensitive_count": imported.report.sensitive.len(),
            "report": imported.report,
        }))?
    );
    Ok(())
}

fn export_profile(database: &Path, arguments: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    let output = take_option(arguments, "--output").map(PathBuf::from);
    let Some(profile_id) = arguments.first() else {
        return Err("export-profile requires a profile UUID".into());
    };
    let profile_id = Uuid::parse_str(profile_id)?;
    let store = SqliteStore::open(database)?;
    let json = store.export_profile_json(profile_id)?;
    if let Some(path) = output {
        fs::write(path, json)?;
    } else {
        println!("{json}");
    }
    Ok(())
}

fn validate_pattern(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(path) = arguments.first() else {
        return Err("validate-pattern requires a .nectar.yaml path".into());
    };
    let source = fs::read_to_string(path)?;
    let program = NectarProgram::from_yaml(&source)?;
    println!(
        "valid: {} ({} top-level steps)",
        program.name,
        program.steps.len()
    );
    Ok(())
}

fn doctor(database: &Path) -> Result<(), Box<dyn Error>> {
    let store = SqliteStore::open(database)?;
    let profiles = store.list_profiles()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "ok",
            "database": store.path(),
            "profiles": profiles.len(),
            "protocol_version": nectarpilot_contracts::PROTOCOL_VERSION,
            "pipe": NamedPipeSpec::for_current_environment().path,
            "roblox_clients": doctor_roblox_clients(),
        }))?
    );
    Ok(())
}

#[cfg(windows)]
fn doctor_roblox_clients() -> serde_json::Value {
    match discover_roblox_clients() {
        Ok(clients) => serde_json::Value::Array(
            clients
                .into_iter()
                .map(|client| {
                    let window = client.window;
                    serde_json::json!({
                        "pid": client.pid.get(),
                        "window_found": window.is_some(),
                        "foreground": window.is_some_and(|snapshot| snapshot.is_foreground),
                        "minimized": window.is_some_and(|snapshot| snapshot.geometry.minimized),
                        "client_width": window.map(|snapshot| snapshot.geometry.client.width),
                        "client_height": window.map(|snapshot| snapshot.geometry.client.height),
                        "dpi": window.map(|snapshot| snapshot.geometry.dpi),
                    })
                })
                .collect(),
        ),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    }
}

#[cfg(not(windows))]
fn doctor_roblox_clients() -> serde_json::Value {
    serde_json::json!({ "error": "Roblox discovery is supported only on Windows" })
}

fn print_pipe_spec() {
    let spec = NamedPipeSpec::for_current_environment();
    println!("path={}", spec.path);
    println!("framing=utf-8-ndjson");
    println!("protocol_version={}", spec.protocol_version);
    println!("max_frame_bytes={}", spec.max_frame_bytes);
    println!("reject_remote_clients={}", spec.reject_remote_clients);
    println!(
        "current_user_acl_required={}",
        spec.current_user_acl_required
    );
}

fn print_help() {
    println!(
        "NectarPilot daemon\n\
         \n\
         Commands:\n\
           serve [--database PATH] [--pipe|--stdio] [--mock-automation]\n\
                                                  Secure user pipe by default on Windows\n\
                                                  Mock normal mode requires an explicit dev flag\n\
           import-ini [--database PATH] [--name NAME] FILE...\n\
           export-profile [--database PATH] [--output PATH] UUID\n\
           validate-pattern FILE.nectar.yaml\n\
           doctor [--database PATH]\n\
           pipe-spec                              Print the Tauri transport contract"
    );
}

fn take_option(arguments: &mut Vec<String>, option: &str) -> Option<String> {
    let index = arguments.iter().position(|value| value == option)?;
    if index + 1 >= arguments.len() {
        arguments.remove(index);
        return None;
    }
    arguments.remove(index);
    Some(arguments.remove(index))
}

fn take_flag(arguments: &mut Vec<String>, flag: &str) -> bool {
    arguments
        .iter()
        .position(|value| value == flag)
        .is_some_and(|index| {
            arguments.remove(index);
            true
        })
}

fn default_database_path() -> PathBuf {
    default_data_directory().join("nectarpilot.sqlite3")
}

fn default_data_directory() -> PathBuf {
    env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("."),
        // Keep CLI/doctor/import on the same current-user directory as the
        // Tauri application (`identifier: com.nectarpilot.desktop`). The
        // desktop always passes this path explicitly, but matching it here
        // prevents a second empty database from confusing diagnostics.
        |local_app_data| PathBuf::from(local_app_data).join("com.nectarpilot.desktop"),
    )
}
