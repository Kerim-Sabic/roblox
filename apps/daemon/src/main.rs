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
    AutomationEngine, MockBackend, SqliteStore,
    dsl::NectarProgram,
    legacy_ini::import_legacy_ini_files,
    transport::{CommandReceiver, EventSender, NamedPipeSpec},
};
use tokio::sync::broadcast;
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};
use uuid::Uuid;

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

    if store
        .runtime_value("daemon_clean_shutdown")?
        .is_some_and(|value| value == "false")
    {
        engine.record_daemon_crash(Utc::now())?;
    }
    store.set_runtime_value("daemon_clean_shutdown", "false")?;
    Ok((store, engine))
}

async fn serve_stdio(database: &Path, allow_mock_automation: bool) -> Result<(), Box<dyn Error>> {
    let (store, engine) = initialize_engine(database, allow_mock_automation)?;

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
                    let _ = engine.handle_command(command).await;
                    if shutdown_requested {
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
                        let _ = engine.handle_command(command).await;
                        if shutdown_requested {
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
        }))?
    );
    Ok(())
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
        |local_app_data| PathBuf::from(local_app_data).join("NectarPilot"),
    )
}
