//! T09 desktop shell. All business operations go through the frozen Host API.

mod awake;
mod backend;
mod settings;
#[cfg(test)]
mod sidecar;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use backend::{CheckView, DeviceView, HostState, LocationView, PairView, StatusView};
use settings::PowerState;
use tauri::{
    Emitter, Manager, State,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::ManagerExt;
use tether_proto::AgentInfo;

#[derive(Default)]
struct ExitState {
    requested: AtomicBool,
    complete: AtomicBool,
}

fn command_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[tauri::command]
async fn pair<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    host: State<'_, HostState>,
) -> Result<PairView, String> {
    let pair = host.pair().await.map_err(command_error)?;
    if let Err(error) = app.emit("desktop:pairing", &pair) {
        tracing::warn!(%error, "pairing event");
    }
    Ok(pair)
}

#[tauri::command]
async fn current_pair(host: State<'_, HostState>) -> Result<Option<PairView>, String> {
    Ok(host.current_pair().await)
}

#[tauri::command]
async fn devices(host: State<'_, HostState>) -> Result<Vec<DeviceView>, String> {
    host.devices().await.map_err(command_error)
}

#[tauri::command]
async fn revoke(host: State<'_, HostState>, id: String) -> Result<(), String> {
    host.revoke(&id).await.map_err(command_error)
}

#[tauri::command]
async fn agents(host: State<'_, HostState>) -> Result<Vec<AgentInfo>, String> {
    host.agents().await.map_err(command_error)
}

#[tauri::command]
async fn status(host: State<'_, HostState>) -> Result<StatusView, String> {
    host.status().await.map_err(command_error)
}

/// The agents whose program can be chosen, with the chosen one (if any).
#[tauri::command]
async fn locations(host: State<'_, HostState>) -> Result<Vec<LocationView>, String> {
    host.locations().await.map_err(command_error)
}

/// Checks the program at `path` (a file or its folder) for `agent` — right
/// name, runs `--version` — then uses it at once and keeps it. Nothing is
/// saved when the check fails.
#[tauri::command]
async fn set_location(
    host: State<'_, HostState>,
    agent: String,
    path: String,
) -> Result<CheckView, String> {
    host.set_location(&agent, &path)
        .await
        .map_err(command_error)
}

/// Forgets `agent`'s chosen program: Tether looks for it by itself again.
#[tauri::command]
async fn clear_location(host: State<'_, HostState>, agent: String) -> Result<(), String> {
    host.clear_location(&agent).await.map_err(command_error)
}

/// A DSH location typed in by an earlier version (kept in the desktop
/// settings) moves to the Host, which now keeps every agent's location.
async fn move_old_dsh_location(host: &HostState, power: &PowerState) {
    let Some(path) = power.dsh_path().await else {
        return;
    };
    if let Err(error) = host.set_location("dsh", &path).await {
        tracing::warn!(%error, path, "earlier DSH location no longer checks out");
    }
    if let Err(error) = power.set_dsh_path(None).await {
        tracing::warn!(%error, "forget earlier DSH location");
    }
}

#[tauri::command]
fn get_keep_awake(power: State<'_, PowerState>) -> bool {
    power.keep_awake()
}

#[tauri::command]
async fn set_keep_awake(power: State<'_, PowerState>, enabled: bool) -> Result<bool, String> {
    power.set_keep_awake(enabled).await.map_err(command_error)
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let result = window
            .unminimize()
            .and_then(|_| window.show())
            .and_then(|_| window.set_focus());
        if let Err(error) = result {
            tracing::error!(%error, "open main window");
        }
    }
}

fn install_tray(app: &tauri::App) -> Result<()> {
    let status_item = MenuItem::with_id(app, "host-status", "正在启动 Host", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "打开窗口", true, None::<&str>)?;
    let pair_item = MenuItem::with_id(app, "pair", "配对新手机", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status_item, &open, &pair_item, &quit])?;
    let icon = app
        .default_window_icon()
        .context("missing bundled tray icon")?
        .clone();
    let tray = TrayIconBuilder::with_id("tether")
        .icon(icon)
        .tooltip("Tether")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "pair" => {
                show_main(app);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    match app.state::<HostState>().pair().await {
                        Ok(pair) => {
                            if let Err(error) = app.emit("desktop:pairing", pair) {
                                tracing::warn!(%error, "tray pairing event");
                            }
                        }
                        Err(error) => {
                            tracing::error!(%error, "tray pairing");
                            let _ = app.emit("desktop:error", command_error(error));
                        }
                    }
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    let app = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        while !app.state::<ExitState>().requested.load(Ordering::Acquire) {
            match app.state::<HostState>().status().await {
                Ok(status) => {
                    let text = backend::tray_text(&status);
                    if let Err(error) = status_item.set_text(&text) {
                        tracing::warn!(%error, "tray status");
                    }
                    if let Err(error) = tray.set_tooltip(Some(&text)) {
                        tracing::warn!(%error, "tray tooltip");
                    }
                }
                Err(error) => tracing::warn!(%error, "poll host status"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    Ok(())
}

fn host_config(app: &tauri::App) -> Result<tether_host::HostConfig> {
    #[cfg(feature = "fake-host")]
    let data_dir = app.path().app_data_dir()?.join("fake-host");
    #[cfg(not(feature = "fake-host"))]
    let data_dir = tether_host::default_data_dir();
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Tether".into());
    let helper_name = if cfg!(windows) {
        "tailpocket-host.exe"
    } else {
        "tailpocket-host"
    };
    let beside_exe = std::env::current_exe()?.with_file_name(helper_name);
    let helper = if beside_exe.is_file() {
        beside_exe
    } else {
        app.path().resource_dir()?.join(helper_name)
    };
    anyhow::ensure!(
        helper.is_file(),
        "missing bundled helper: {}",
        helper.display()
    );
    Ok(tether_host::HostConfig {
        data_dir,
        name,
        tailpocket_helper: Some(helper),
        ..Default::default()
    })
}

pub fn run() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app)
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name("Tether")
                .build(),
        )
        .manage(ExitState::default())
        .invoke_handler(tauri::generate_handler![
            pair,
            current_pair,
            devices,
            revoke,
            agents,
            status,
            get_keep_awake,
            set_keep_awake,
            locations,
            set_location,
            clear_location
        ])
        .setup(|app| {
            let power = PowerState::load(app.path().app_data_dir()?.join("desktop-settings.json"))?;
            let host = tauri::async_runtime::block_on(HostState::start(host_config(app)?))?;
            if let Err(error) = tauri::async_runtime::block_on(async {
                power
                    .initialize_autostart(|| app.autolaunch().enable().map_err(Into::into))
                    .await?;
                power.apply_saved().await
            }) {
                // Host::start succeeded: balance it even when subsequent setup fails.
                let _ = tauri::async_runtime::block_on(host.shutdown());
                return Err(error.into());
            }
            tauri::async_runtime::block_on(move_old_dsh_location(&host, &power));
            app.manage(host);
            app.manage(power);
            if let Err(error) = install_tray(app) {
                let _ = tauri::async_runtime::block_on(app.state::<HostState>().shutdown());
                app.state::<PowerState>().shutdown();
                return Err(error.into());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && !window
                    .state::<ExitState>()
                    .requested
                    .load(Ordering::Acquire)
            {
                api.prevent_close();
                if let Err(error) = window.hide() {
                    tracing::error!(%error, "hide window to tray");
                }
            }
        })
        .build(tauri::generate_context!())?;
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            let exit = app.state::<ExitState>();
            if exit.complete.load(Ordering::Acquire) {
                return;
            }
            api.prevent_exit();
            if exit.requested.swap(true, Ordering::AcqRel) {
                return;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let mut exit_code = code.unwrap_or(0);
                if let Err(error) = app.state::<HostState>().shutdown().await {
                    tracing::error!(%error, "Host shutdown");
                    exit_code = 1;
                }
                app.state::<PowerState>().shutdown();
                app.state::<ExitState>()
                    .complete
                    .store(true, Ordering::Release);
                app.exit(exit_code);
            });
        }
    });
    Ok(())
}

#[cfg(all(test, feature = "fake-host"))]
mod command_tests {
    use super::*;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[tokio::test]
    async fn registered_host_command_functions_use_managed_fake_host() {
        let host = HostState::start(tether_host::HostConfig::default())
            .await
            .unwrap();
        let app = mock_builder()
            .manage(host)
            .build(mock_context(noop_assets()))
            .unwrap();
        assert!(current_pair(app.state()).await.unwrap().is_none());
        let qr = pair(app.handle().clone(), app.state()).await.unwrap();
        assert!(qr.qr.starts_with("tether1:"));
        assert_eq!(current_pair(app.state()).await.unwrap(), Some(qr));
        assert_eq!(devices(app.state()).await.unwrap().len(), 2);
        assert_eq!(agents(app.state()).await.unwrap().len(), 3);
        assert_eq!(status(app.state()).await.unwrap().sessions_need_you, 2);
        revoke(app.state(), "fake-pixel".into()).await.unwrap();
        assert_eq!(devices(app.state()).await.unwrap().len(), 1);
        assert_eq!(status(app.state()).await.unwrap().connected_devices, 0);
        assert!(revoke(app.state(), "".into()).await.is_err());
        app.state::<HostState>().shutdown().await.unwrap();
        assert_eq!(devices(app.state()).await.unwrap_err(), "Host 已停止");
        assert!(pair(app.handle().clone(), app.state()).await.is_err());
    }
}
