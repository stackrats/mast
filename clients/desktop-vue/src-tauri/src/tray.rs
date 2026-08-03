//! Tray subsystem (M8): mirrors engine state — workspace submenus with
//! Start/Stop all, every project under one "Projects" entry (two menu
//! levels max: some hosts drop deeper nesting), rebuilt from a fresh
//! snapshot on every relevant patch.

use futures::StreamExt;
use mast_client::MastClient;
use mast_contract::{Action, ProjectId, SubscriptionItem, WorkspaceId};
use tauri::{AppHandle, Manager};

use crate::AppState;

/// Bring the main window back from close-to-tray: unhide, undo any minimise,
/// then focus.
fn reveal_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    #[cfg(target_os = "linux")]
    {
        // A tray menu click arrives over DBus, so the plain activation above
        // carries no X11 user timestamp and KWin/Mutter decline it — the
        // window only gets flagged in the taskbar. Presenting with a freshly
        // read server time is the timestamp their focus-stealing prevention
        // is asking for. (Pinning always-on-top instead does raise it, but
        // unpinning drops it straight back down the stack.)
        use gtk::glib::Cast;
        use gtk::prelude::{GtkWindowExt, WidgetExt};

        if let Ok(gtk_window) = window.gtk_window() {
            match gtk_window.window().and_then(|w| w.downcast::<gdkx11::X11Window>().ok()) {
                Some(x11) => {
                    gtk_window.present_with_time(gdkx11::functions::x11_get_server_time(&x11))
                }
                // Wayland: no X11 handle, and the compositor decides anyway.
                None => gtk_window.present(),
            }
        }
    }
}

pub fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::Manager;
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    let open = MenuItemBuilder::with_id("open", "Open Mast").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("bundled icon"))
        .tooltip("Mast")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "open" => reveal_window(app),
                "quit" => app.exit(0),
                "stopall" => {
                    let client = app.state::<AppState>().client.clone();
                    tauri::async_runtime::spawn(async move {
                        let Ok(snapshot) = client.snapshot().await else { return };
                        for project in snapshot
                            .projects
                            .iter()
                            .filter(|p| p.status != mast_contract::ProjectStatus::Stopped)
                        {
                            let action = Action::StopProject { id: project.id.clone() };
                            if let Err(e) = client.dispatch(action).await {
                                tracing::warn!("tray stop-all failed for {}: {e}", project.id.0);
                            }
                        }
                    });
                }
                other => {
                    // Per-project lifecycle entries: "start:{id}" etc. The op
                    // streams to any open UI; tray fire-and-forget is fine —
                    // outcomes surface through observed state.
                    if let Some((verb, target)) = other.split_once(':') {
                        let id = ProjectId(target.to_string());
                        let action = match verb {
                            "start" => Action::StartProject { id },
                            "stop" => Action::StopProject { id },
                            "restart" => Action::RestartProject { id },
                            "wsstart" => {
                                Action::StartWorkspace { id: WorkspaceId(target.to_string()) }
                            }
                            "wsstop" => {
                                Action::StopWorkspace { id: WorkspaceId(target.to_string()) }
                            }
                            _ => return,
                        };
                        let client = app.state::<AppState>().client.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = client.dispatch(action).await {
                                tracing::warn!("tray action failed: {e}");
                            }
                        });
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

/// M8: the tray mirrors engine state — per-project submenus with lifecycle
/// verbs and a status glyph, rebuilt from a fresh snapshot on every relevant
/// patch (resubscribing on ResyncRequired like any other client).
pub async fn refresh_loop(app: AppHandle) {
    use mast_contract::PatchEvent;

    loop {
        let client = {
            let state: tauri::State<'_, AppState> = app.state();
            state.client.clone()
        };
        let mut stream = match client.subscribe(None).await {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!("tray subscription failed: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if let Err(e) = rebuild_tray(&app, client.as_ref()).await {
            tracing::warn!("tray rebuild failed: {e}");
        }
        while let Some(item) = stream.next().await {
            match item {
                SubscriptionItem::Patch { patch } => {
                    let relevant = matches!(
                        patch.event,
                        PatchEvent::ProjectAdded { .. }
                            | PatchEvent::ProjectUpdated { .. }
                            | PatchEvent::ProjectStatusChanged { .. }
                            | PatchEvent::ProjectRemoved { .. }
                            | PatchEvent::DockerStatusChanged { .. }
                            | PatchEvent::WorkspacesChanged { .. }
                    );
                    if relevant && let Err(e) = rebuild_tray(&app, client.as_ref()).await {
                        tracing::warn!("tray rebuild failed: {e}");
                    }
                }
                SubscriptionItem::ResyncRequired => break,
            }
        }
    }
}

/// Colored status dots matching the GUI's chip dots (menus can't render
/// real widgets, but color-font circles read identically).
fn status_glyph(status: mast_contract::ProjectStatus) -> &'static str {
    use mast_contract::ProjectStatus;
    match status {
        ProjectStatus::Running => "🟢",
        ProjectStatus::Starting => "🟠",
        ProjectStatus::Degraded | ProjectStatus::Failed => "🔴",
        _ => "⚪",
    }
}

fn project_submenu(
    app: &AppHandle,
    project: &mast_contract::ProjectSummary,
    label: &str,
) -> tauri::Result<tauri::menu::Submenu<tauri::Wry>> {
    use mast_contract::ProjectStatus;
    use tauri::menu::{MenuItemBuilder, SubmenuBuilder};

    let start = MenuItemBuilder::with_id(format!("start:{}", project.id.0), "Start")
        .enabled(project.status != ProjectStatus::Running)
        .build(app)?;
    let stop = MenuItemBuilder::with_id(format!("stop:{}", project.id.0), "Stop")
        .enabled(project.status != ProjectStatus::Stopped)
        .build(app)?;
    let restart = MenuItemBuilder::with_id(format!("restart:{}", project.id.0), "Restart")
        .enabled(project.status == ProjectStatus::Running)
        .build(app)?;
    SubmenuBuilder::new(app, label).items(&[&start, &stop, &restart]).build()
}

async fn rebuild_tray(app: &AppHandle, client: &dyn MastClient) -> tauri::Result<()> {
    use mast_contract::ProjectStatus;
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    let Ok(snapshot) = client.snapshot().await else { return Ok(()) };
    let Some(tray) = app.tray_by_id("main") else { return Ok(()) };

    // Two levels only: some tray hosts (GNOME appindicator) drop third-level
    // submenus, which hid workspace members. Workspaces get their own
    // submenu with Start/Stop all; EVERY project is a top-level submenu,
    // members labelled with their workspace.
    let workspace_of: std::collections::HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .flat_map(|w| w.members.iter().map(|m| (m.project.0.as_str(), w.name.as_str())))
        .collect();

    let mut builder = MenuBuilder::new(app);
    for workspace in &snapshot.workspaces {
        let start_all =
            MenuItemBuilder::with_id(format!("wsstart:{}", workspace.id.0), "Start all")
                .enabled(workspace.status != ProjectStatus::Running
                    && workspace.graph_error.is_none())
                .build(app)?;
        let stop_all = MenuItemBuilder::with_id(format!("wsstop:{}", workspace.id.0), "Stop all")
            .enabled(workspace.status != ProjectStatus::Stopped)
            .build(app)?;
        let ws_menu = SubmenuBuilder::new(
            app,
            format!("{} {} (workspace)", status_glyph(workspace.status), workspace.name),
        )
        .items(&[&start_all, &stop_all])
        .build()?;
        builder = builder.item(&ws_menu);
    }
    if !snapshot.workspaces.is_empty() {
        builder = builder.separator();
    }
    // All projects live under one "Projects" entry so a long list of loose
    // projects cannot bloat the tray (workspaces above already expand).
    if !snapshot.projects.is_empty() {
        let running = snapshot
            .projects
            .iter()
            .filter(|p| p.status == ProjectStatus::Running)
            .count();
        let mut projects_builder = SubmenuBuilder::new(
            app,
            format!("Projects ({running}/{} running)", snapshot.projects.len()),
        );
        for project in &snapshot.projects {
            let label = match workspace_of.get(project.id.0.as_str()) {
                Some(workspace) => {
                    format!("{} {} · {workspace}", status_glyph(project.status), project.name)
                }
                None => format!("{} {}", status_glyph(project.status), project.name),
            };
            let submenu = project_submenu(app, project, &label)?;
            projects_builder = projects_builder.item(&submenu);
        }
        let projects_menu = projects_builder.build()?;
        builder = builder.item(&projects_menu);
    }
    if !snapshot.projects.is_empty() || !snapshot.workspaces.is_empty() {
        // One-shot panic button: stops every project that isn't already
        // stopped, whether or not it belongs to a workspace.
        let any_live =
            snapshot.projects.iter().any(|p| p.status != ProjectStatus::Stopped);
        let stop_all =
            MenuItemBuilder::with_id("stopall", "Stop all projects").enabled(any_live).build(app)?;
        builder = builder.item(&stop_all).separator();
    }
    let open = MenuItemBuilder::with_id("open", "Open Mast").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = builder.items(&[&open, &quit]).build()?;
    tray.set_menu(Some(menu))?;

    let running =
        snapshot.projects.iter().filter(|p| p.status == ProjectStatus::Running).count();
    let unhealthy = snapshot
        .projects
        .iter()
        .filter(|p| matches!(p.status, ProjectStatus::Degraded | ProjectStatus::Failed))
        .count();
    let mut tooltip = format!("Mast — {running}/{} running", snapshot.projects.len());
    if unhealthy > 0 {
        tooltip.push_str(&format!(", {unhealthy} unhealthy"));
    }
    if !snapshot.docker.available {
        tooltip.push_str(" · docker offline");
    }
    tray.set_tooltip(Some(tooltip))?;
    Ok(())
}
