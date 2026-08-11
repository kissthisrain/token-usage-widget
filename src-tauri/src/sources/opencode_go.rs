// OpenCode Go 套餐适配器（一期：仅本地估算 + 价目表；官方抓取在二期）
// 对齐 core/sources/opencode-go.js

use crate::config::{Price, PriceMap, SourceConfig};
use crate::history::{HistoryStore, QuotaInfo, QuotaItem};
use once_cell::sync::Lazy;
use std::collections::HashMap;

// 官方价目表（USD / 1M tokens，2026-08 抓取自 opencode.ai/zh/go）
// 注：cost_for/价目表供二期本地代理计价使用
#[allow(dead_code)]
pub fn default_price_map() -> PriceMap {
    [
        ("grok-4.5", 2.0, 6.0),
        ("gpt-5.6-luna", 0.2, 1.2),
        ("glm-5.2", 1.4, 4.4),
        ("glm-5.1", 1.4, 4.4),
        ("glm-5", 1.4, 4.4),
        ("kimi-k3", 3.0, 15.0),
        ("kimi-k2.7-code", 0.95, 4.0),
        ("kimi-k2.6", 0.95, 4.0),
        ("kimi-k2.5", 0.95, 4.0),
        ("mimo-v2.5", 0.14, 0.28),
        ("mimo-v2.5-pro", 0.435, 0.87),
        ("mimo-v2-pro", 0.435, 0.87),
        ("mimo-v2-omni", 0.14, 0.28),
        ("minimax-m3", 0.3, 1.2),
        ("minimax-m2.7", 0.3, 1.2),
        ("minimax-m2.5", 0.3, 1.2),
        ("qwen3.8-max", 2.0, 6.0),
        ("qwen3.7-max", 2.5, 7.5),
        ("qwen3.7-plus", 0.4, 1.6),
        ("qwen3.6-plus", 0.5, 3.0),
        ("qwen3.5-plus", 0.5, 3.0),
        ("deepseek-v4-pro", 0.435, 0.87),
        ("deepseek-v4-flash", 0.14, 0.28),
        ("hy3", 0.14, 0.58),
        ("hy3-preview", 0.14, 0.58),
    ]
    .iter()
    .map(|(k, i, o)| ((*k).to_string(), Price { input: *i, output: *o }))
    .collect()
}

#[allow(dead_code)]
pub static DEFAULT_PRICE_MAP: Lazy<PriceMap> = Lazy::new(default_price_map);

// 官方限额
pub struct Limit {
    pub key: &'static str,
    pub label: &'static str,
    pub limit_usd: f64,
    pub window_ms: i64, // 0 = 日历月
}
pub static LIMITS: Lazy<Vec<Limit>> = Lazy::new(|| {
    vec![
        Limit { key: "5h", label: "5 小时", limit_usd: 12.0, window_ms: 5 * 3600 * 1000 },
        Limit { key: "week", label: "每周", limit_usd: 30.0, window_ms: 7 * 86400 * 1000 },
        Limit { key: "month", label: "每月", limit_usd: 60.0, window_ms: 0 },
    ]
});

#[allow(dead_code)]
pub fn model_key(model: &str) -> String {
    model.trim_start_matches("opencode-go/").to_string()
}

/// 计算一次请求费用（美元），未知模型返回 null
#[allow(dead_code)]
pub fn cost_for(model: &str, input: f64, output: f64, price_map: &PriceMap) -> Option<f64> {
    let m = model_key(model);
    let p = price_map.get(&m).or_else(|| DEFAULT_PRICE_MAP.get(&m))?;
    Some((input / 1e6) * p.input + (output / 1e6) * p.output)
}

/// 解析源里的 priceMap 字段 → PriceMap（内部字段 {input, output}）
#[allow(dead_code)]
pub fn source_price_map(src: &SourceConfig) -> PriceMap {
    let mut map = HashMap::new();
    if let Some(pm) = &src.price_map {
        for (k, v) in pm {
            if let Some(obj) = v.as_object() {
                let input = obj.get("input").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let output = obj.get("output").and_then(|x| x.as_f64()).unwrap_or(0.0);
                map.insert(k.clone(), Price { input, output });
            }
        }
    }
    map
}

fn month_end_ts(now: i64) -> i64 {
    use chrono::{Datelike, Local, TimeZone};
    let d = Local.timestamp_millis_opt(now).single().unwrap_or_else(Local::now);
    let next = Local.with_ymd_and_hms(d.year(), d.month() + 1, 1, 0, 0, 0).single();
    next.map(|dt| dt.timestamp_millis()).unwrap_or(now)
}

#[allow(dead_code)]
fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

/// 依据历史记录计算三个限额窗口的剩余%（动态计算，无需请求）
pub fn compute_quota(history: &HistoryStore, source: &SourceConfig) -> QuotaInfo {
    let now = crate::util::now_ms();
    let mut items = Vec::new();
    for lim in LIMITS.iter() {
        let (spent, partial, first_ts) = if lim.window_ms > 0 {
            history.spent_in_window(&source.id, lim.window_ms)
        } else {
            let (spent, partial) = history.spent_in_calendar_month(&source.id);
            (spent, partial, None)
        };
        let resets_at = if lim.window_ms > 0 {
            first_ts.map(|f| f + lim.window_ms)
        } else {
            Some(month_end_ts(now))
        };
        let remaining = (lim.limit_usd - spent).max(0.0);
        items.push(QuotaItem {
            key: Some(lim.key.to_string()),
            label: Some(lim.label.to_string()),
            limit: Some(lim.limit_usd),
            spent: Some(spent),
            remaining: Some(remaining),
            used_pct: if lim.limit_usd > 0.0 {
                Some(((spent / lim.limit_usd) * 100.0).min(100.0))
            } else {
                Some(0.0)
            },
            resets_at,
            currency: Some("USD".to_string()),
            partial: Some(partial),
            ..Default::default()
        });
    }
    QuotaInfo {
        source_id: Some(source.id.clone()),
        mode: Some("opencode-go-limits".to_string()),
        items,
        local: Some(true),
        currency: Some("USD".to_string()),
        at: now,
        ..Default::default()
    }
}

/// 解析控制台重置时间的自然语言文本 → 秒（"6 days 2 hours" / "3h 42m" / "6 天 2 小时"），失败返回 None
fn parse_human_duration(text: &str) -> Option<i64> {
    let t = String::from(text).trim().to_string();
    if t.is_empty() {
        return None;
    }
    if !t.chars().any(|c| c.is_ascii_digit()) {
        return if regex::Regex::new(r"(?i)reset|now|立即|马上")
            .map(|re| re.is_match(&t))
            .unwrap_or(false)
        {
            Some(0)
        } else {
            None
        };
    }
    let tokens: [(&str, i64); 7] = [
        (r"(?i)(?:day|天)", 86400),
        (r"(?i)(?:hour|小时)", 3600),
        (r"(?i)(?:min|分钟)", 60),
        (r"(?i)(?:sec|秒)", 1),
        (r"\bh\b", 3600),
        (r"\bm\b", 60),
        (r"\bs\b", 1),
    ];
    let mut total: Option<i64> = None;
    for (pat, sec) in tokens {
        if let Ok(re) = regex::Regex::new(&format!(r"(\d+)\s*{}", pat)) {
            for cap in re.captures_iter(&t) {
                if let Ok(n) = cap[1].parse::<i64>() {
                    total = Some(total.unwrap_or(0) + n * sec);
                }
            }
        }
    }
    total
}

// 控制台页面窗口名 → 限额 key
const SSR_NAMES: [(&str, &str); 3] = [("rollingUsage", "5h"), ("weeklyUsage", "week"), ("monthlyUsage", "month")];

#[derive(Clone, Debug, Default)]
struct UsageWin {
    used_pct: Option<f64>,
    resets_at: Option<i64>,
}

fn clamp_pct(p: f64) -> Option<f64> {
    if !p.is_finite() {
        return None;
    }
    Some(p.clamp(0.0, 100.0))
}

// 控制台页面解析用正则（编译一次复用）
static RE_USAGE_PCT: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"usagePercent["']?\s*[:=]\s*["']?([\d.]+)"#).unwrap());
static RE_RESET_SEC: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"resetInSec["']?\s*[:=]\s*["']?(\d+)"#).unwrap());
static RE_SLOT_LABEL: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"data-slot="usage-label">([^<]*)"#).unwrap());
static RE_SLOT_VALUE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"data-slot="usage-value">([^<]*)"#).unwrap());
static RE_SLOT_RESET: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"data-slot="reset-time">([^<]*)"#).unwrap());
static RE_ROLLING: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)rolling|5\s*hour|滚动|5\s*小时").unwrap());
static RE_WEEKLY: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)weekly|week|每周").unwrap());
static RE_MONTHLY: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)monthly|month|每月").unwrap());

/// 从控制台页面 HTML 提取三个窗口的用量百分比与重置时间
/// 两种内嵌格式：
///  1) SSR 水合 JSON：rollingUsage:$R[n]={usagePercent:23.5,resetInSec:13320}
///  2) data-slot HTML：<div data-slot="usage-item">...
fn extract_usage(html: &str) -> std::collections::HashMap<String, UsageWin> {
    let text = String::from(html);
    let mut out: std::collections::HashMap<String, UsageWin> = std::collections::HashMap::new();
    for (name, key) in SSR_NAMES {
        let Some(idx) = text.find(name) else { continue };
        let seg = &text[idx..(idx + 400).min(text.len())];
        let pct = RE_USAGE_PCT
            .captures(seg)
            .and_then(|c| c[1].parse::<f64>().ok())
            .and_then(clamp_pct);
        let sec = RE_RESET_SEC
            .captures(seg)
            .and_then(|c| c[1].parse::<i64>().ok());
        if let Some(p) = pct {
            out.insert(
                key.to_string(),
                UsageWin {
                    used_pct: Some(p),
                    resets_at: sec.map(|s| crate::util::now_ms() + s * 1000),
                },
            );
        }
    }
    // data-slot 兜底（水合数据缺失时）
    if out.len() < SSR_NAMES.len() {
        let slots: Vec<(String, String, String)> = text
            .split("data-slot=\"usage-item\"")
            .skip(1)
            .map(|b| {
                let label = RE_SLOT_LABEL
                    .captures(b)
                    .map(|c| c[1].to_string())
                    .unwrap_or_default();
                let value = RE_SLOT_VALUE
                    .captures(b)
                    .map(|c| c[1].to_string())
                    .unwrap_or_default();
                let reset = RE_SLOT_RESET
                    .captures(b)
                    .map(|c| c[1].to_string())
                    .or_else(|| {
                        if b.contains("data-slot=\"reset-now\"") {
                            Some("0 seconds".to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                (label, value, reset)
            })
            .filter(|(_, value, _)| value.parse::<f64>().ok().and_then(clamp_pct).is_some())
            .collect();
        for (order, (label, value, reset)) in slots.into_iter().enumerate() {
            let by_label = if RE_ROLLING.is_match(&label) {
                Some("5h")
            } else if RE_WEEKLY.is_match(&label) {
                Some("week")
            } else if RE_MONTHLY.is_match(&label) {
                Some("month")
            } else {
                None
            };
            let key = by_label.map(|s| s.to_string()).or_else(|| {
                if order < SSR_NAMES.len() {
                    Some(SSR_NAMES[order].1.to_string())
                } else {
                    None
                }
            });
            let Some(key) = key else { continue };
            if out.contains_key(&key) {
                continue;
            }
            let sec = parse_human_duration(&reset);
            let pct = value.parse::<f64>().ok().and_then(clamp_pct);
            if let Some(p) = pct {
                out.insert(
                    key,
                    UsageWin {
                        used_pct: Some(p),
                        resets_at: sec.map(|s| crate::util::now_ms() + s * 1000),
                    },
                );
            }
        }
    }
    out
}

/// 官方用量查询结果
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // 部分字段由调用方按需读取
pub struct ProbeQuotaResult {
    pub ok: bool,
    pub source_id: Option<String>,
    pub mode: Option<String>,
    pub items: Vec<QuotaItem>,
    pub currency: Option<String>,
    pub official: Option<bool>,
    pub error: Option<String>,
}

/// 官方用量查询：GET https://opencode.ai/workspace/{workspaceId}/go（带登录 Cookie）
pub async fn probe_quota(source: &SourceConfig) -> ProbeQuotaResult {
    let workspace_id = source.workspace_id.clone().unwrap_or_default();
    let workspace_id = workspace_id.trim().to_string();
    let cookie = source.auth_cookie.clone().unwrap_or_default();
    let cookie = cookie.trim().to_string();
    if workspace_id.is_empty() || cookie.is_empty() {
        return ProbeQuotaResult {
            ok: false,
            error: Some("未配置工作区 ID / Auth Cookie（设置 → OpenCode Go → 官方用量查询）".to_string()),
            ..Default::default()
        };
    }
    let cookie = if cookie.to_lowercase().starts_with("auth=") {
        cookie
    } else {
        format!("auth={cookie}")
    };
    let url = format!(
        "https://opencode.ai/workspace/{}/go",
        urlencoding_encode(&workspace_id)
    );
    let headers = vec![("Cookie".to_string(), cookie)];
    let res = match crate::http::get_json(&url, &headers, 15000).await {
        Ok(r) => r,
        Err(e) => {
            return ProbeQuotaResult {
                ok: false,
                error: Some(format!("请求失败: {e}")),
                ..Default::default()
            }
        }
    };
    // 未登录会被 302 到 /auth/authorize
    let loc = res
        .header("location")
        .map(|s| s.to_string())
        .unwrap_or_default();
    if res.status == 302
        || res.status == 401
        || loc.contains("/auth/authorize")
    {
        return ProbeQuotaResult {
            ok: false,
            error: Some("会话已过期或 Cookie 无效，请重新登录 opencode.ai 后复制最新 auth Cookie".to_string()),
            ..Default::default()
        };
    }
    if res.status >= 400 {
        return ProbeQuotaResult {
            ok: false,
            error: Some(format!("HTTP {}", res.status)),
            ..Default::default()
        };
    }
    let usage = extract_usage(&res.body_text);
    if !usage.contains_key("5h") && !usage.contains_key("week") && !usage.contains_key("month") {
        return ProbeQuotaResult {
            ok: false,
            error: Some("页面已返回但未解析到用量数据（页面结构可能已变更）".to_string()),
            ..Default::default()
        };
    }
    let mut items = Vec::new();
    for lim in LIMITS.iter() {
        let Some(u) = usage.get(lim.key) else { continue };
        let Some(pct) = u.used_pct else { continue };
        let spent = round4((lim.limit_usd * pct) / 100.0);
        items.push(QuotaItem {
            key: Some(lim.key.to_string()),
            label: Some(lim.label.to_string()),
            limit: Some(lim.limit_usd),
            spent: Some(spent),
            remaining: Some((lim.limit_usd - spent).max(0.0)),
            used_pct: Some(pct),
            resets_at: u.resets_at,
            currency: Some("USD".to_string()),
            official: Some(true),
            ..Default::default()
        });
    }
    if items.is_empty() {
        return ProbeQuotaResult {
            ok: false,
            error: Some("未解析到有效用量项".to_string()),
            ..Default::default()
        };
    }
    ProbeQuotaResult {
        ok: true,
        source_id: Some(source.id.clone()),
        mode: Some("opencode-go-official".to_string()),
        items,
        currency: Some("USD".to_string()),
        official: Some(true),
        ..Default::default()
    }
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 设置界面测试：官方额度源 → 真实抓取；代理源 → 配置说明（旧合并类型按字段判断兼容）
pub async fn test(source: &SourceConfig, _history: &HistoryStore) -> serde_json::Value {
    let official = source.type_ == "opencode-go-official"
        || source.type_ == "opencode-go"
        || (source.workspace_id.is_some() && source.auth_cookie.is_some());
    if official {
        let r = probe_quota(source).await;
        if r.ok {
            let parts: Vec<String> = r
                .items
                .iter()
                .map(|i| {
                    format!(
                        "{} 已用 {:.1}% / 剩余 ${:.2}",
                        i.label.clone().unwrap_or_default(),
                        i.used_pct.unwrap_or(0.0),
                        i.remaining.unwrap_or(0.0)
                    )
                })
                .collect();
            return serde_json::json!({ "ok": true, "message": format!("官方用量: {}", parts.join("；")) });
        }
        return serde_json::json!({
            "ok": false,
            "error": format!("官方用量查询失败: {}", r.error.unwrap_or_default()),
        });
    }
    let mut msgs = Vec::new();
    if source.port.is_none() || source.port == Some(0) {
        msgs.push("未配置代理端口".to_string());
    }
    let base = source
        .base_url
        .clone()
        .unwrap_or_else(|| "https://opencode.ai/zen/go".to_string())
        .trim_end_matches('/')
        .to_string();
    msgs.push(format!("代理目标: {base}"));
    msgs.push("限额: 5h $12 / 周 $30 / 月 $60".to_string());
    msgs.push(format!(
        "启动代理后，将客户端 API Base URL 指向 http://127.0.0.1:{}{}",
        source.port.map(|p| p.to_string()).unwrap_or_else(|| "—".to_string()),
        ""
    ));
    msgs.push("配置「工作区 ID + Auth Cookie」可获取官方剩余用量（见下方字段）".to_string());
    serde_json::json!({ "ok": true, "message": msgs.join("；") })
}
