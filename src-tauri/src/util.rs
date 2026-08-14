// 通用工具：日期/数字处理（对齐 core/util.js 的主进程侧纯函数）

use chrono::{DateTime, Datelike, Local, TimeZone};

pub fn pad2(n: i32) -> String {
    format!("{n:02}")
}

/// 时间戳(ms) → 本地日期 'YYYY-MM-DD'
pub fn date_key(ts: i64) -> String {
    let dt = Local
        .timestamp_millis_opt(ts)
        .single()
        .unwrap_or_else(Local::now);
    format!("{}-{}-{}", dt.year(), pad2(dt.month() as i32), pad2(dt.day() as i32))
}

pub fn today_key() -> String {
    date_key(now_ms())
}

/// 今天 00:00 的毫秒时间戳（本地时区）
pub fn today_start_ms() -> i64 {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now();
    Local
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(now_ms)
}

/// 天数（含当天）之前的日期 key
pub fn days_ago_key(days: i64) -> String {
    date_key(now_ms() - days * 86400000)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 解析时间：数字(秒自动×1000) 或 ISO 字符串 → ms；失败返回 None
pub fn parse_ts(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64().map(|f| {
            if f < 1e12 {
                (f * 1000.0) as i64
            } else {
                f as i64
            }
        }),
        serde_json::Value::String(s) => parse_ts_str(s),
        _ => None,
    }
}

fn parse_ts_str(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // ISO 8601（含 Z / 时区偏移）
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    // 兼容无时区的常见格式
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(naive.and_utc().timestamp_millis());
        }
    }
    None
}

pub fn err_msg(status: u16, body: &serde_json::Value) -> String {
    if let Some(obj) = body.as_object() {
        for k in ["error", "message", "msg"] {
            if let Some(e) = obj.get(k) {
                if let Some(s) = e.as_str() {
                    return format!("HTTP {status}: {}", &s[..s.len().min(200)]);
                }
                if let Some(msg) = e.get("message").and_then(|m| m.as_str()) {
                    return format!("HTTP {status}: {}", &msg[..msg.len().min(200)]);
                }
            }
        }
    }
    format!("HTTP {status}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_key_works() {
        // 2026-08-10 12:00:00 UTC+8
        let ts = 1754793600000; // 2026-08-10 08:00:00 UTC → 本地 +8
        let k = date_key(ts);
        assert_eq!(k.len(), 10);
        let y = k[0..4].parse::<i32>().unwrap();
        assert!((2024..=2035).contains(&y));
    }

    #[test]
    fn parse_ts_variants() {
        assert_eq!(parse_ts(&serde_json::json!(1700000000i64)), Some(1700000000000));
        assert_eq!(parse_ts(&serde_json::json!(1700000000000i64)), Some(1700000000000));
        assert!(parse_ts(&serde_json::json!("2026-08-10T08:00:00Z")).is_some());
        assert!(parse_ts(&serde_json::Value::Null).is_none());
    }
}
