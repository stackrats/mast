//! Thin Tauri layer: every command delegates to `mast-client` (never to the
//! engine directly). The desktop owns the engine locally AND serves it over
//! the daemon socket, so the CLI shares one engine instead of conflicting.
//!
//! Transport split per plan §3: low-volume state patches go out as a global
//! event; per-operation progress uses a dedicated Tauri channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use mast_client::MastClient;
use mast_client_local::LocalClient;
use mast_contract::{
    Action, CatalogEntry, CustomServiceSpec, DiagnosticReport, DiagnosticsHistory, EngineSnapshot,
    EnvReport, LaravelLogReport, PhpRuntimeReport, ProxyCa, FileEditPreview, HistoryEntry, LogCapture, LogLine, OperationEvent, OperationId,
    ProjectId, RepairPlan, SnapshotReport, SubscriptionItem, UsageSample, WorkspaceId,
    WorkspaceSnapshot,
};
use mast_engine::{Engine, EngineConfig, EngineDeps, RealConnector, RealLifecycleRunner};
use mast_project::MetadataStore;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;

mod tray;

pub struct AppState {
    pub(crate) client: Arc<dyn MastClient>,
    /// The single active patch-forwarder; replaced (and aborted) on each
    /// (re)subscribe so a resync can never interleave two streams.
    patch_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Active log-stream forwarders, keyed by handle for explicit stop.
    log_streams: Mutex<HashMap<u32, tauri::async_runtime::JoinHandle<()>>>,
    next_log_stream: AtomicU32,
    /// The single active history forwarder; replaced (and aborted) on
    /// resubscribe, like the patch task.
    history_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    capture_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    usage_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

/// Global event carrying patch-stream items. `stream_id` lets the frontend
/// discard stragglers from a superseded subscription generation.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct PatchStreamItem {
    pub stream_id: u32,
    pub item: SubscriptionItem,
}

#[tauri::command]
#[specta::specta]
async fn snapshot(state: State<'_, AppState>) -> Result<EngineSnapshot, String> {
    state.client.snapshot().await.map_err(|e| e.to_string())
}

/// Start (or replace) the patch subscription. Items arrive as
/// [`PatchStreamItem`] events tagged with `stream_id`.
#[tauri::command]
#[specta::specta]
async fn start_patch_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    stream_id: u32,
    after_seq: Option<u64>,
) -> Result<(), String> {
    let mut stream = state.client.subscribe(after_seq).await.map_err(|e| e.to_string())?;
    let task = tauri::async_runtime::spawn(async move {
        while let Some(item) = stream.next().await {
            let last = matches!(item, SubscriptionItem::ResyncRequired);
            let _ = PatchStreamItem { stream_id, item }.emit(&app);
            if last {
                break;
            }
        }
    });
    if let Some(old) = state.patch_task.lock().unwrap().replace(task) {
        old.abort();
    }
    Ok(())
}

/// Dispatch any action; the operation's full event history (replayed from the
/// first event) streams over `on_event` until a terminal event.
#[tauri::command]
#[specta::specta]
async fn dispatch_action(
    state: State<'_, AppState>,
    action: Action,
    on_event: Channel<OperationEvent>,
) -> Result<OperationId, String> {
    let client = state.client.clone();
    let id = client.dispatch(action).await.map_err(|e| e.to_string())?;
    let mut events = client.operation_events(id).await.map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.next().await {
            let terminal = event.kind.is_terminal();
            if on_event.send(event).is_err() {
                break;
            }
            if terminal {
                break;
            }
        }
    });
    Ok(id)
}

#[tauri::command]
#[specta::specta]
async fn cancel_operation(state: State<'_, AppState>, id: OperationId) -> Result<(), String> {
    state.client.cancel(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn env_report(state: State<'_, AppState>, project: ProjectId) -> Result<EnvReport, String> {
    state.client.env_report(project).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn laravel_log(
    state: State<'_, AppState>,
    project: ProjectId,
) -> Result<LaravelLogReport, String> {
    state.client.laravel_log(project).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn proxy_ca(state: State<'_, AppState>) -> Result<Option<ProxyCa>, String> {
    state.client.proxy_ca().await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn php_runtime(
    state: State<'_, AppState>,
    project: ProjectId,
) -> Result<PhpRuntimeReport, String> {
    state.client.php_runtime(project).await.map_err(|e| e.to_string())
}

/// The effect-history backlog (M9): what Mast has run and written, oldest
/// first. Fetched once at connect; [`start_history_stream`] keeps it current.
#[tauri::command]
#[specta::specta]
async fn history_recent(state: State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> {
    state.client.history_recent().await.map_err(|e| e.to_string())
}

/// Follow effect history. Entries arrive on creation and again on completion;
/// the frontend upserts by id. Replaces any earlier subscription.
#[tauri::command]
#[specta::specta]
async fn start_history_stream(
    state: State<'_, AppState>,
    on_entry: Channel<HistoryEntry>,
) -> Result<(), String> {
    let mut stream = state.client.subscribe_history().await.map_err(|e| e.to_string())?;
    let task = tauri::async_runtime::spawn(async move {
        while let Some(entry) = stream.next().await {
            if on_entry.send(entry).is_err() {
                break;
            }
        }
    });
    if let Some(old) = state.history_task.lock().unwrap().replace(task) {
        old.abort();
    }
    Ok(())
}

/// Stored log captures (M10), newest first: what each container was saying
/// just before it went down. Read from disk, so this survives an app restart.
#[tauri::command]
#[specta::specta]
async fn log_captures(state: State<'_, AppState>, limit: u32) -> Result<Vec<LogCapture>, String> {
    state.client.log_captures(limit).await.map_err(|e| e.to_string())
}

/// Follow log captures. Append-only — the frontend prepends. Replaces any
/// earlier subscription.
#[tauri::command]
#[specta::specta]
async fn start_capture_stream(
    state: State<'_, AppState>,
    on_capture: Channel<LogCapture>,
) -> Result<(), String> {
    let mut stream = state.client.subscribe_log_captures().await.map_err(|e| e.to_string())?;
    let task = tauri::async_runtime::spawn(async move {
        while let Some(capture) = stream.next().await {
            if on_capture.send(capture).is_err() {
                break;
            }
        }
    });
    if let Some(old) = state.capture_task.lock().unwrap().replace(task) {
        old.abort();
    }
    Ok(())
}

/// Follow live CPU/memory usage (M11). **Calling this is what starts the
/// engine sampling** — it makes no docker calls while nobody is subscribed, so
/// the frontend stops the stream whenever the window is hidden. Replaces any
/// earlier subscription.
#[tauri::command]
#[specta::specta]
async fn start_usage_stream(
    state: State<'_, AppState>,
    on_sample: Channel<UsageSample>,
) -> Result<(), String> {
    let mut stream = state.client.subscribe_usage().await.map_err(|e| e.to_string())?;
    let task = tauri::async_runtime::spawn(async move {
        while let Some(sample) = stream.next().await {
            if on_sample.send(sample).is_err() {
                break;
            }
        }
    });
    if let Some(old) = state.usage_task.lock().unwrap().replace(task) {
        old.abort();
    }
    Ok(())
}

/// Drop the usage subscription, which stops the engine sampling.
#[tauri::command]
#[specta::specta]
async fn stop_usage_stream(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(task) = state.usage_task.lock().unwrap().take() {
        task.abort();
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
async fn list_snapshots(
    state: State<'_, AppState>,
    workspace: WorkspaceId,
) -> Result<Vec<WorkspaceSnapshot>, String> {
    state.client.list_snapshots(workspace).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn snapshot_report(
    state: State<'_, AppState>,
    snapshot_id: String,
) -> Result<SnapshotReport, String> {
    state.client.snapshot_report(snapshot_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn catalog(
    state: State<'_, AppState>,
    project: ProjectId,
) -> Result<Vec<CatalogEntry>, String> {
    state.client.catalog(project).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn catalog_preview(
    state: State<'_, AppState>,
    project: ProjectId,
    service: String,
    remove: bool,
) -> Result<FileEditPreview, String> {
    state.client.catalog_preview(project, service, remove).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn service_remove_preview(
    state: State<'_, AppState>,
    project: ProjectId,
    service: String,
) -> Result<FileEditPreview, String> {
    state.client.service_remove_preview(project, service).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn service_image_preview(
    state: State<'_, AppState>,
    project: ProjectId,
    service: String,
    image: String,
) -> Result<FileEditPreview, String> {
    state.client.service_image_preview(project, service, image).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn custom_service_preview(
    state: State<'_, AppState>,
    project: ProjectId,
    spec: CustomServiceSpec,
) -> Result<FileEditPreview, String> {
    state.client.custom_service_preview(project, spec).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn run_diagnostics(
    state: State<'_, AppState>,
    project: Option<ProjectId>,
) -> Result<DiagnosticReport, String> {
    state.client.run_diagnostics(project).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn repair_preview(
    state: State<'_, AppState>,
    repair: String,
    arg: Option<String>,
    project: Option<ProjectId>,
) -> Result<RepairPlan, String> {
    state.client.repair_preview(repair, arg, project).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn diagnostics_history(state: State<'_, AppState>) -> Result<DiagnosticsHistory, String> {
    state.client.diagnostics_history().await.map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
async fn network_attach_preview(
    state: State<'_, AppState>,
    workspace: WorkspaceId,
    project: ProjectId,
) -> Result<FileEditPreview, String> {
    state.client.network_attach_preview(workspace, project).await.map_err(|e| e.to_string())
}

/// Follow a service's container logs over a dedicated channel (plan §3: log
/// streams bypass the patch store). Returns a handle for `stop_log_stream`.
#[tauri::command]
#[specta::specta]
async fn stream_service_logs(
    state: State<'_, AppState>,
    project: ProjectId,
    service: String,
    tail: u32,
    on_line: Channel<LogLine>,
) -> Result<u32, String> {
    let mut stream = state
        .client
        .service_logs(project, service, tail)
        .await
        .map_err(|e| e.to_string())?;
    let task = tauri::async_runtime::spawn(async move {
        while let Some(line) = stream.next().await {
            if on_line.send(line).is_err() {
                break;
            }
        }
    });
    let handle = state.next_log_stream.fetch_add(1, Ordering::Relaxed) + 1;
    state.log_streams.lock().unwrap().insert(handle, task);
    Ok(handle)
}

#[tauri::command]
#[specta::specta]
async fn stop_log_stream(state: State<'_, AppState>, handle: u32) -> Result<(), String> {
    if let Some(task) = state.log_streams.lock().unwrap().remove(&handle) {
        task.abort();
    }
    Ok(())
}

fn specta_builder() -> tauri_specta::Builder {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            snapshot,
            start_patch_stream,
            dispatch_action,
            cancel_operation,
            env_report,
            laravel_log,
            proxy_ca,
            php_runtime,
            network_attach_preview,
            list_snapshots,
            snapshot_report,
            run_diagnostics,
            repair_preview,
            diagnostics_history,
            catalog,
            catalog_preview,
            service_remove_preview,
            service_image_preview,
            custom_service_preview,
            stream_service_logs,
            stop_log_stream,
            history_recent,
            start_history_stream,
            log_captures,
            start_capture_stream,
            start_usage_stream,
            stop_usage_stream,
        ])
        .events(tauri_specta::collect_events![PatchStreamItem])
}

pub fn run() {
    let builder = specta_builder();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        // Autostarted instances launch minimized to the tray.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .invoke_handler(builder.invoke_handler())
        .on_window_event(|window, event| {
            // Close-to-tray: the engine keeps observing in the background.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(move |app| {
            if let Err(e) = tray::setup_tray(app) {
                tracing::warn!("tray unavailable: {e}");
            }
            if std::env::args().any(|arg| arg == "--minimized")
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }
            let store = MetadataStore::open(MetadataStore::default_dir())?;
            let engine = Engine::new(
                EngineConfig::default(),
                EngineDeps {
                    connector: Arc::new(RealConnector),
                    store,
                    process_env: std::env::vars().collect(),
                    runner: Arc::new(RealLifecycleRunner),
                    // Second instances run read-only (plan §1); flock releases
                    // automatically if the owner dies.
                    ownership: mast_engine::acquire_ownership(None),
                },
            );
            {
                let engine = engine.clone();
                tauri::async_runtime::spawn(async move {
                    engine.start();
                    // Serve the shared-engine socket (M8) when this instance
                    // owns mutation — the CLI then gets full rights instead
                    // of a read-only second engine.
                    if !engine.snapshot().read_only
                        && let Err(e) =
                            mast_daemon::serve(engine, &mast_daemon::default_socket_path()).await
                    {
                        tracing::warn!("ipc socket unavailable: {e}");
                    }
                });
            }
            app.manage(AppState {
                client: Arc::new(LocalClient::new(engine)),
                patch_task: Mutex::new(None),
                log_streams: Mutex::new(HashMap::new()),
                next_log_stream: AtomicU32::new(0),
                history_task: Mutex::new(None),
                capture_task: Mutex::new(None),
                usage_task: Mutex::new(None),
            });
            builder.mount_events(app);
            tauri::async_runtime::spawn(tray::refresh_loop(app.handle().clone()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running mast-desktop");
}

#[cfg(test)]
mod tests {
    /// Regenerates the committed TS bindings; CI runs the full test suite, so
    /// stale bindings show up as a diff in review.
    #[test]
    fn export_typescript_bindings() {
        super::specta_builder()
            .export(
                specta_typescript::Typescript::default()
                    // u64 seq/operation counters stay far below 2^53; JSON
                    // carries them as numbers already, so `number` is exact.
                    .bigint(specta_typescript::BigIntExportBehavior::Number)
                    .header("// @ts-nocheck — generated by tauri-specta; do not edit\n"),
                "../src/bindings.ts",
            )
            .expect("failed to export typescript bindings");
    }
}
