// Tauri 应用装配：插件 / 状态 / 服务 / 窗口 / 托盘 / 热键 / 自启 / 命令

mod commands;
mod config;
mod detect;
mod events;
mod history;
mod http;
mod proxy;
mod scheduler;
mod smoke;
mod snapshot;
mod sources;
mod sqlite_import;
mod util;
mod watcher;
mod windows;

use config::{ConfigStore, SourceConfig};
use history::HistoryStore;
use scheduler::Scheduler;
use std::sync::{Arc, Mutex};
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use std::str::FromStr;
use watcher::Watcher;
use windows::WindowMgr;

pub struct AppState {
    pub config: Arc<ConfigStore>,
    pub history: Arc<HistoryStore>,
    pub watcher: Arc<Watcher>,
    pub scheduler: Arc<Scheduler>,
    pub win_mgr: Arc<WindowMgr>,
    pub go_proxy: Arc<proxy::GoProxy>,
    pub hotkey_registered: Mutex<Option<Shortcut>>,
}

pub fn run() {
    // 冒烟测试：无界面验证数据管道（对齐 Electron --smoke）
    if std::env::args().any(|a| a == "--smoke") {
        smoke::run();
        return;
    }
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .write_style(env_logger::WriteStyle::Never)
    .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(st) = app.try_state::<AppState>() {
                st.win_mgr.show_widget();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let data_dir = config::user_data_dir();
            let config = Arc::new(ConfigStore::new(&data_dir));
            config.load();
            let history_store = Arc::new(HistoryStore::new(&data_dir));
            history_store.load();

            let app_handle = app.handle().clone();
            let get_sources: Arc<dyn Fn() -> Vec<SourceConfig> + Send + Sync> = {
                let config = config.clone();
                Arc::new(move || config.get_sources(false))
            };
            let watcher = Arc::new(Watcher::new(
                history_store.clone(),
                get_sources,
                app_handle.clone(),
            ));
            let scheduler = Arc::new(Scheduler::new(
                config.clone(),
                history_store.clone(),
                app_handle.clone(),
            ));
            let win_mgr = Arc::new(WindowMgr::new(app_handle.clone(), config.clone()));
            let go_proxy = Arc::new(proxy::GoProxy::new(
                {
                    let config = config.clone();
                    Arc::new(move || {
                        config
                            .get_sources(false)
                            .into_iter()
                            .find(|s| s.type_ == "opencode-go")
                    })
                },
                history_store.clone(),
                app_handle.clone(),
            ));

            let state = AppState {
                config: config.clone(),
                history: history_store.clone(),
                watcher: watcher.clone(),
                scheduler: scheduler.clone(),
                win_mgr: win_mgr.clone(),
                go_proxy: go_proxy.clone(),
                hotkey_registered: Mutex::new(None),
            };
            app.manage(state);

            // 核心服务
            watcher.restart();
            scheduler.start();
            // OpenCode Go 本地代理（源启用时自动启动）
            {
                let st = app.state::<AppState>();
                if let Some(src) = st.go_proxy.source() {
                    if src.enabled {
                        let gp = st.go_proxy.clone();
                        tauri::async_runtime::spawn(async move {
                            gp.start().await;
                        });
                    }
                }
            }

            // 窗口
            win_mgr.create_widget();
            win_mgr.apply_opacity();
            // 调试：--open-settings 启动即打开设置窗口
            if std::env::args().any(|a| a == "--open-settings") {
                win_mgr.open_settings();
            }

            // 托盘
            build_tray(app)?;

            // 热键 & 自启
            let app_handle = app.handle();
            let st = app.state::<AppState>();
            if let Err(e) = register_hotkey(app_handle, &st) {
                log::warn!("热键注册失败: {e}");
            }
            if let Err(e) = apply_autolaunch(app_handle, &st) {
                log::warn!("开机自启设置失败: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 外部链接（页面 target=_blank）用系统浏览器打开
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "widget" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::get_config,
            commands::save_config,
            commands::test_source,
            commands::refresh_all,
            commands::detect_scan,
            commands::get_autolaunch,
            commands::set_autolaunch,
            commands::clear_data,
            commands::export_data,
            commands::proxy_status,
            commands::proxy_start,
            commands::proxy_stop,
            commands::win_open_settings,
            commands::win_toggle,
            commands::win_resize,
            commands::win_get_pinned,
            commands::win_set_pinned,
            commands::win_minimize,
            commands::win_close,
            commands::app_quit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 配置保存后重启核心服务（对齐 reinitCore）
pub fn reinit_core(app: &AppHandle, state: &AppState) {
    state.watcher.restart();
    state.scheduler.start();
    // OpenCode Go 代理：源启用且未运行 → 启动；禁用或删除 → 停止
    let src = state.go_proxy.source();
    let should_run = src.as_ref().map(|s| s.enabled).unwrap_or(false);
    if should_run && !state.go_proxy.is_running() {
        let gp = state.go_proxy.clone();
        tauri::async_runtime::spawn(async move {
            gp.start().await;
        });
    } else if !should_run && state.go_proxy.is_running() {
        state.go_proxy.stop();
    }
    if let Err(e) = register_hotkey(app, state) {
        log::warn!("热键重注册失败: {e}");
    }
    if let Err(e) = apply_autolaunch(app, state) {
        log::warn!("开机自启设置失败: {e}");
    }
    state.win_mgr.apply_opacity();
    let _ = app.emit(
        "data:update",
        events::DataUpdate { reinit: Some(true), ..Default::default() },
    );
}

/// 全局热键注册（Electron 加速键 → Tauri 加速键）
pub fn register_hotkey(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let gs = app.global_shortcut();
    if let Some(old) = state.hotkey_registered.lock().unwrap().take() {
        let _ = gs.unregister(old);
    }
    let Some(acc) = state.config.hotkey_accelerator() else {
        return Ok(());
    };
    let shortcut = Shortcut::from_str(&acc).map_err(|e| format!("无效热键 {acc}: {e}"))?;
    let sc = shortcut;
    gs.on_shortcut(sc, |app, _shortcut, event| {
        if event.state() == ShortcutState::Pressed {
            if let Some(st) = app.try_state::<AppState>() {
                st.win_mgr.toggle_widget();
            }
        }
    })
    .map_err(|e| format!("注册失败: {e}"))?;
    *state.hotkey_registered.lock().unwrap() = Some(shortcut);
    Ok(())
}

/// 应用开机自启（对齐 applyAutoLaunch）
pub fn apply_autolaunch(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let v = state.config.get_general().auto_launch;
    if v {
        app.autolaunch().enable().map_err(|e| e.to_string())?;
    } else {
        let _ = app.autolaunch().disable();
    }
    Ok(())
}

pub fn autolaunch_current(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

pub fn set_autolaunch_value(app: &AppHandle, v: bool) -> Result<commands::OkResult, String> {
    if v {
        app.autolaunch().enable().map_err(|e| e.to_string())?;
    } else {
        let _ = app.autolaunch().disable();
    }
    Ok(commands::OkResult { ok: true, error: None, path: None })
}

/// 系统托盘
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let toggle = MenuItemBuilder::with_id("toggle", "显示 / 隐藏").build(app)?;
    let settings_item = MenuItemBuilder::with_id("settings", "设置…").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[
            &toggle,
            &settings_item,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ])
        .build()?;

    let on_menu = move |app: &AppHandle, event: tauri::menu::MenuEvent| match event.id().as_ref() {
        "toggle" => {
            if let Some(st) = app.try_state::<AppState>() {
                st.win_mgr.toggle_widget();
            }
        }
        "settings" => {
            if let Some(st) = app.try_state::<AppState>() {
                st.win_mgr.open_settings();
            }
        }
        "quit" => {
            app.exit(0);
        }
        _ => {}
    };
    let on_tray = move |tray: &tauri::tray::TrayIcon, event: TrayIconEvent| {
        // 只响应左键单击（右键会弹出菜单，若也触发 toggle 会抢焦点导致菜单闪退）
        if let TrayIconEvent::Click {
            button: tauri::tray::MouseButton::Left,
            ..
        } = event
        {
            let app = tray.app_handle();
            if let Some(st) = app.try_state::<AppState>() {
                st.win_mgr.toggle_widget();
            }
        }
    };

    let mut tray_builder = TrayIconBuilder::with_id("main")
        .tooltip("Token 用量小组件")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu)
        .on_tray_icon_event(on_tray);
    // Windows 托盘必须显式设置图标，否则任务栏不显示
    if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../../resources/icon.png")) {
        tray_builder = tray_builder.icon(icon);
    }
    tray_builder.build(app)?;
    Ok(())
}

// 热键按下处理（on_shortcut 在 register_hotkey 内注册，handler 直接绑定到具体热键）
