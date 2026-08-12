// Tauri 命令层：对齐 main.js 的 IPC handler 清单（preload 30 方法 → 命令）
// 事件通道 data:update / app:opacity 由 events.rs 与 windows.rs 广播

use crate::config::{Config, SourceConfig};
use crate::detect::Detection;
use crate::events::DataUpdate;
use crate::snapshot::{build_snapshot, Snapshot};
use crate::sources::{compat, openai, opencode_go};
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, Window};

fn range_default() -> String {
    "month".to_string()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotOpts {
    #[serde(default = "range_default")]
    pub range: String,
    #[serde(default)]
    pub source_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OkResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[tauri::command]
pub async fn get_snapshot(
    state: State<'_, AppState>,
    opts: Option<SnapshotOpts>,
) -> Result<Snapshot, String> {
    let o = opts.unwrap_or_default();
    Ok(build_snapshot(
        &state.config,
        &state.history,
        if o.range.is_empty() { "month" } else { &o.range },
        if o.source_id.is_empty() { "all" } else { &o.source_id },
    ))
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    Ok(state.config.load())
}

#[tauri::command]
pub async fn save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    cfg: Config,
) -> Result<OkResult, String> {
    state.config.replace_and_save(cfg);
    crate::reinit_core(&app, &state);
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn test_source(
    state: State<'_, AppState>,
    cfg: SourceConfig,
) -> Result<serde_json::Value, String> {
    let r = match cfg.type_.as_str() {
        "openai" => openai::test(&cfg).await,
        "compat" => compat::test(&cfg).await,
        "opencode-go-official" | "opencode-go-proxy" | "opencode-go" => {
            opencode_go::test(&cfg, &state.history).await
        }
        "local-logs" => {
            let p = cfg.path.clone().unwrap_or_default();
            let ok = !p.is_empty() && std::path::Path::new(&p).exists();
            if ok {
                serde_json::json!({ "ok": true, "message": format!("路径存在：{p}") })
            } else {
                serde_json::json!({ "ok": false, "error": format!("路径不存在：{}", if p.is_empty() { "(未填写)" } else { &p }) })
            }
        }
        other => serde_json::json!({ "ok": false, "error": format!("未知类型：{other}") }),
    };
    Ok(r)
}

#[tauri::command]
pub async fn refresh_all(state: State<'_, AppState>) -> Result<OkResult, String> {
    state.scheduler.tick().await;
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn detect_scan(_state: State<'_, AppState>) -> Result<Vec<Detection>, String> {
    Ok(crate::detect::scan_all())
}

#[tauri::command]
pub async fn get_autolaunch(app: AppHandle) -> bool {
    crate::autolaunch_current(&app)
}

#[tauri::command]
pub async fn set_autolaunch(
    app: AppHandle,
    state: State<'_, AppState>,
    v: bool,
) -> Result<OkResult, String> {
    let r = crate::set_autolaunch_value(&app, v);
    let mut c = state.config.get();
    c.general.auto_launch = v;
    state.config.replace_and_save(c);
    r
}

#[tauri::command]
pub async fn clear_data(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<OkResult, String> {
    state.history.clear();
    let _ = app.emit(
        "data:update",
        DataUpdate { reinit: Some(true), ..Default::default() },
    );
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn export_data(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<OkResult, String> {
    use tauri_plugin_dialog::DialogExt;
    let default_dir = dirs::document_dir().unwrap_or_else(crate::config::user_data_dir);
    let default_path = default_dir.join(format!("token-usage-export-{}.json", crate::util::now_ms()));
    let file_name = default_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "token-usage-export.json".to_string());
    let file_path = app
        .dialog()
        .file()
        .add_filter("JSON", &["json"])
        .set_file_name(file_name)
        .blocking_save_file();
    let Some(fp) = file_path else {
        return Ok(OkResult { ok: false, error: Some("已取消".to_string()), path: None });
    };
    let path = fp.into_path().map_err(|e| e.to_string())?;
    let cfg = state.config.get();
    let hist = state.history.data();
    let payload = serde_json::json!({
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "config": cfg,
        "history": {
            "days": hist.days,
            "recent": hist.recent,
            "quota": hist.quota,
        },
    });
    let json = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(OkResult { ok: true, error: None, path: Some(path.to_string_lossy().to_string()) })
}

// ---- 代理（OpenCode Go 本地代理） ----

#[tauri::command]
pub async fn proxy_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    Ok(state.go_proxy.status())
}

#[tauri::command]
pub async fn proxy_start(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let ok = state.go_proxy.start().await;
    Ok(serde_json::json!({ "ok": ok, "status": state.go_proxy.status() }))
}

#[tauri::command]
pub async fn proxy_stop(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.go_proxy.stop();
    Ok(serde_json::json!({ "ok": true, "status": state.go_proxy.status() }))
}

// ---- 窗口控制（async：窗口创建/尺寸调整等 WebView2 操作不能在主线程阻塞，
//       否则异步完成回调无法到达 → 死锁；异步命令跑在线程池，主线程继续 pump） ----

#[tauri::command]
pub async fn win_open_settings(state: State<'_, AppState>) -> Result<OkResult, String> {
    state.win_mgr.open_settings();
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn win_toggle(state: State<'_, AppState>) -> Result<OkResult, String> {
    state.win_mgr.toggle_widget();
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn win_resize(state: State<'_, AppState>, h: f64) -> Result<OkResult, String> {
    state.win_mgr.resize_to_content(h);
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn win_get_pinned(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.win_mgr.get_pinned())
}

#[tauri::command]
pub async fn win_set_pinned(state: State<'_, AppState>, v: bool) -> Result<bool, String> {
    Ok(state.win_mgr.set_pinned(v))
}

#[tauri::command]
pub async fn win_minimize(window: Window) -> Result<OkResult, String> {
    let _ = window.minimize();
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn win_close(window: Window) -> Result<OkResult, String> {
    let _ = window.close();
    Ok(OkResult { ok: true, error: None, path: None })
}

// ---- 贴边收缩 ----

#[tauri::command]
pub async fn win_show_mini(state: State<'_, AppState>) -> Result<OkResult, String> {
    state.win_mgr.show_mini();
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn win_show_main(state: State<'_, AppState>) -> Result<OkResult, String> {
    state.win_mgr.show_main();
    Ok(OkResult { ok: true, error: None, path: None })
}

#[tauri::command]
pub async fn app_quit(app: AppHandle) -> Result<OkResult, String> {
    app.exit(0);
    Ok(OkResult { ok: true, error: None, path: None })
}
