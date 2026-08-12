// 配置读写：%APPDATA%/token-usage-widget/config.json
// schema 与 core/config.js 完全对齐（camelCase），未知字段经 flatten 保留，无损往返

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DEFAULT_HOTKEY: &str = "CommandOrControl+Alt+U";

fn default_version() -> u64 {
    1
}
fn default_true() -> bool {
    true
}
fn default_interval() -> i64 {
    10
}
fn default_hotkey() -> String {
    DEFAULT_HOTKEY.to_string()
}
fn default_opacity() -> f64 {
    0.94
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeneralConfig {
    #[serde(default = "default_interval")]
    pub refresh_interval_min: i64,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_true")]
    pub auto_launch: bool,
    #[serde(default = "default_true")]
    pub show_cost: bool,
    #[serde(default = "default_true")]
    pub show_quota: bool,
    #[serde(default = "default_true")]
    pub show_charts: bool,
    #[serde(default = "default_true")]
    pub show_model_list: bool,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default)]
    pub always_on_top: bool,
    // 贴边收缩模式
    #[serde(default = "default_true")]
    pub dock_enabled: bool,
    #[serde(default = "default_dock_metric")]
    pub dock_metric: String,
    #[serde(default = "default_true")]
    pub dock_always_on_top: bool,
}

fn default_dock_metric() -> String {
    "go-remaining".to_string()
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowBounds {
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
}

/// 数据源配置（字段与 renderer/js/settings.js 的 DEFAULTS 及 core/config.js 对齐）
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourceConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub display_order: i64,
    #[serde(default)]
    pub model_whitelist: Vec<String>,
    #[serde(rename = "type")]
    pub type_: String,
    // local-logs
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: Option<bool>,
    #[serde(default)]
    pub file_pattern: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub import_sqlite: Option<bool>,
    #[serde(default)]
    pub sqlite_db: Option<String>,
    // openai / compat
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub organization_id: Option<String>,
    #[serde(default)]
    pub balance_mode: Option<String>,
    #[serde(default)]
    pub custom_balance_url: Option<String>,
    // opencode-go
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub price_map: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub auth_cookie: Option<String>,
    // 未知字段无损保留（前端可能写入新字段）
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl SourceConfig {
    pub fn str_field(&self, v: &Option<String>) -> String {
        v.as_deref().unwrap_or("").to_string()
    }
    pub fn bool_field(&self, v: &Option<bool>, default: bool) -> bool {
        v.unwrap_or(default)
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u64,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub window_bounds: Option<WindowBounds>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 应用数据目录：沿用 Electron 版 %APPDATA%/token-usage-widget（数据无缝迁移）
pub fn user_data_dir() -> PathBuf {
    if let Ok(a) = std::env::var("APPDATA") {
        PathBuf::from(a).join("token-usage-widget")
    } else if let Some(d) = dirs::data_dir() {
        d.join("token-usage-widget")
    } else {
        PathBuf::from(".").join("token-usage-widget")
    }
}

pub struct ConfigStore {
    pub file: PathBuf,
    pub data: Mutex<Config>,
}

impl ConfigStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            file: dir.join("config.json"),
            data: Mutex::new(Config::default()),
        }
    }

    /// 读盘 + 默认值合并；sources 为空数组时首启自动写盘（对齐 JS load()）
    pub fn load(&self) -> Config {
        let loaded: Option<Config> = std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let mut cfg = loaded.unwrap_or_default();
        let empty_sources = cfg.sources.is_empty();
        // 一次性迁移：旧「opencode-go」单类型拆分为「官方额度 + 本地代理」两个独立源
        let migrated = migrate_opencode_go_split(&mut cfg);
        *self.data.lock().unwrap() = cfg.clone();
        if empty_sources || migrated {
            self.save();
        }
        cfg
    }

    pub fn save(&self) {
        let cfg = self.data.lock().unwrap().clone();
        let json = serde_json::to_string_pretty(&cfg).unwrap_or_default();
        let _ = atomic_write(&self.file, &json);
    }

    /// 整体替换并写盘（config:save 语义，对齐 JS 全量回传）
    pub fn replace_and_save(&self, cfg: Config) {
        *self.data.lock().unwrap() = cfg;
        self.save();
    }

    pub fn get(&self) -> Config {
        self.data.lock().unwrap().clone()
    }

    pub fn get_general(&self) -> GeneralConfig {
        self.get().general
    }

    pub fn get_sources(&self, enabled_only: bool) -> Vec<SourceConfig> {
        let cfg = self.get();
        if enabled_only {
            cfg.sources.into_iter().filter(|s| s.enabled).collect()
        } else {
            cfg.sources
        }
    }

    pub fn get_source(&self, id: &str) -> Option<SourceConfig> {
        self.get().sources.into_iter().find(|s| s.id == id)
    }

    /// 定位到全局热键加速键（Electron 格式 → Tauri 格式）
    pub fn hotkey_accelerator(&self) -> Option<String> {
        let hk = self.get_general().hotkey.trim().to_string();
        if hk.is_empty() {
            return None;
        }
        let mut s = hk;
        s = s.replace("CommandOrControl", "CmdOrCtrl");
        s = s.replace("CommandOrCtrl", "CmdOrCtrl");
        Some(s)
    }

    pub fn gen_id(&self, prefix: &str) -> String {
        let now = now_b36();
        let rnd: String = {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            (0..5)
                .map(|_| {
                    let c = rng.gen_range(0..36);
                    if c < 10 {
                        (b'0' + c as u8) as char
                    } else {
                        (b'a' + (c - 10) as u8) as char
                    }
                })
                .collect()
        };
        format!("{prefix}_{now}{rnd}")
    }

    pub fn sources_by_type(&self, ty: &str) -> Vec<SourceConfig> {
        self.get_sources(false).into_iter().filter(|s| s.type_ == ty).collect()
    }
}

/// 迁移：旧「opencode-go」单类型拆分为两个独立源：
/// - opencode-go-proxy（本地代理）：保留原 id（历史消耗记录归属它，零丢失）
/// - opencode-go-official（官方额度）：生成新 id（配额数据本就独立挂载）
pub fn migrate_opencode_go_split(cfg: &mut Config) -> bool {
    let mut changed = false;
    let mut new_sources: Vec<SourceConfig> = Vec::with_capacity(cfg.sources.len() + 1);
    for s in cfg.sources.drain(..) {
        if s.type_ != "opencode-go" {
            new_sources.push(s);
            continue;
        }
        changed = true;
        // 代理部分：保留原 id 与代理相关字段
        let mut proxy = s.clone();
        proxy.type_ = "opencode-go-proxy".to_string();
        proxy.workspace_id = None;
        proxy.auth_cookie = None;
        new_sources.push(proxy);
        // 官方部分：新 id，仅当原配置了官方字段
        if s.workspace_id.is_some() || s.auth_cookie.is_some() {
            let mut official = s.clone();
            official.id = format!("{}_official", s.id);
            official.type_ = "opencode-go-official".to_string();
            official.port = None;
            official.base_url = None;
            official.price_map = None;
            new_sources.push(official);
        }
    }
    cfg.sources = new_sources;
    changed
}

fn now_b36() -> String {
    let mut n = crate::util::now_ms();
    let mut s = String::new();
    if n == 0 {
        return "0".to_string();
    }
    while n > 0 {
        let d = (n % 36) as u8;
        s.push(if d < 10 { (b'0' + d) as char } else { (b'a' + (d - 10)) as char });
        n /= 36;
    }
    s.chars().rev().collect()
}

/// 原子写：tmp + rename（Windows 下 fs::rename 可覆盖已存在文件）
pub fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 价格表结构（opencode-go 用；二期本地代理计价）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[allow(dead_code)]
pub struct Price {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
}

#[allow(dead_code)]
pub type PriceMap = HashMap<String, Price>;
