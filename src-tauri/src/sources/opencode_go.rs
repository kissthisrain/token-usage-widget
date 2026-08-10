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

/// 设置界面测试（一期：未配置官方查询时给出配置说明；官方用在二期）
pub async fn test(source: &SourceConfig, _history: &HistoryStore) -> serde_json::Value {
    let official = source.workspace_id.is_some() && source.auth_cookie.is_some();
    if official {
        // 二期实现官方抓取前给出占位
        return serde_json::json!({
            "ok": false,
            "error": "官方用量查询将在二期版本提供（当前版本使用本地代理估算）",
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
    msgs.push("配置「工作区 ID + Auth Cookie」可获取官方剩余用量（见下方字段，二期支持）".to_string());
    serde_json::json!({ "ok": true, "message": msgs.join("；") })
}
