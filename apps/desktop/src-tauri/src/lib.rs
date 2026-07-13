mod bridge;

use std::path::Path;
use std::sync::{Arc, Mutex};

use bridge::{DaemonBridge, DispatchReceipt, UiAutomationSettings};
use nectarpilot_contracts::{Command, CommandEnvelope, RunSnapshot};
use serde::Serialize;
use serde_json::Value;
use tauri::{Manager, PhysicalSize, Size, WebviewWindow};
use uuid::Uuid;

#[derive(Default)]
struct ShellState {
    compact: Mutex<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellInfo {
    app_name: &'static str,
    version: String,
    developer_checkout: bool,
    compact: bool,
    updater_enabled: bool,
}

fn contains_git_directory(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor.join(".git").is_dir())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
fn get_shell_info(
    app: tauri::AppHandle,
    state: tauri::State<'_, ShellState>,
) -> Result<ShellInfo, String> {
    let compact = *state
        .compact
        .lock()
        .map_err(|_| "shell state lock poisoned".to_owned())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let developer_checkout = contains_git_directory(&executable)
        || std::env::current_dir().is_ok_and(|directory| contains_git_directory(&directory));

    Ok(ShellInfo {
        app_name: "NectarPilot",
        version: app.package_info().version.to_string(),
        developer_checkout,
        compact,
        // Developer checkouts never self-update. Packaged builds use the
        // signed stable or beta endpoint compiled into their Tauri config.
        updater_enabled: !developer_checkout,
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
fn set_compact_mode(
    window: WebviewWindow,
    state: tauri::State<'_, ShellState>,
    compact: bool,
) -> Result<(), String> {
    let size = if compact {
        PhysicalSize::new(380, 220)
    } else {
        PhysicalSize::new(1040, 720)
    };

    window
        .set_min_size(Some(Size::Physical(size)))
        .map_err(|error| error.to_string())?;
    window
        .set_size(Size::Physical(size))
        .map_err(|error| error.to_string())?;
    *state
        .compact
        .lock()
        .map_err(|_| "shell state lock poisoned".to_owned())? = compact;
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window is unavailable".to_owned())?;
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn get_dashboard_snapshot(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
) -> Result<Value, String> {
    Ok(bridge.dashboard_snapshot().await)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
fn get_run_snapshot(bridge: tauri::State<'_, Arc<DaemonBridge>>) -> Option<RunSnapshot> {
    bridge.cached_run()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn dispatch_command(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    envelope: CommandEnvelope,
) -> Result<DispatchReceipt, String> {
    if matches!(
        &envelope.command,
        Command::ShutdownDaemon | Command::StartLegacy { .. }
    ) {
        return Err("restricted daemon commands are unavailable to the WebView".to_owned());
    }
    bridge.dispatch(envelope).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn select_profile(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
) -> Result<(), String> {
    bridge.select_profile(profile_id).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn save_automation_settings(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
    settings: UiAutomationSettings,
) -> Result<(), String> {
    bridge.save_automation_settings(profile_id, settings).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn complete_onboarding(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
) -> Result<(), String> {
    bridge.complete_onboarding(profile_id).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn trust_extension(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
    extension_id: String,
    digest: String,
) -> Result<(), String> {
    bridge
        .trust_extension(profile_id, extension_id, digest)
        .await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn start_legacy_extension(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
    extension_id: String,
    digest: String,
) -> Result<(), String> {
    bridge
        .start_legacy_extension(profile_id, extension_id, digest)
        .await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn start_legacy_session(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
    max_cycles: u32,
    max_minutes: u32,
) -> Result<(), String> {
    bridge
        .start_legacy_session(profile_id, max_cycles, max_minutes)
        .await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn inspect_legacy(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
    script_id: String,
) -> Result<(), String> {
    bridge.inspect_legacy(profile_id, script_id).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn import_secret(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
    name: String,
    value: String,
) -> Result<(), String> {
    bridge.import_secret(profile_id, name, value).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn scan_quests(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
) -> Result<(), String> {
    bridge.scan_quests(profile_id).await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands own extractor values.
async fn get_run_history(
    bridge: tauri::State<'_, Arc<DaemonBridge>>,
    profile_id: Uuid,
) -> Result<(), String> {
    bridge.get_run_history(profile_id).await
}

pub fn run() {
    let application = tauri::Builder::default()
        .manage(ShellState::default())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let bridge = DaemonBridge::new(app.handle().clone());
            bridge.start();
            app.manage(bridge);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_shell_info,
            set_compact_mode,
            show_main_window,
            get_dashboard_snapshot,
            get_run_snapshot,
            dispatch_command,
            select_profile,
            save_automation_settings,
            complete_onboarding,
            trust_extension,
            start_legacy_extension,
            start_legacy_session,
            inspect_legacy,
            import_secret,
            scan_quests,
            get_run_history
        ])
        .build(tauri::generate_context!())
        .expect("NectarPilot desktop shell failed to build");

    application.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } => {
            let bridge = Arc::clone(app.state::<Arc<DaemonBridge>>().inner());
            if !bridge.is_shutting_down() {
                api.prevent_exit();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    bridge.shutdown().await;
                    app.exit(0);
                });
            }
        }
        tauri::RunEvent::Exit => {
            app.state::<Arc<DaemonBridge>>().force_stop_owned_daemon();
        }
        _ => {}
    });
}
