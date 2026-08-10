// OpenAI 兼容中转站适配器：余额/用量接口自动探测（对齐 core/sources/compat.js）

use crate::config::SourceConfig;
use crate::history::QuotaItem;
use crate::http::get_json;
use serde_json::Value;

fn auth_headers(source: &SourceConfig) -> Vec<(String, String)> {
    vec![(
        "Authorization".to_string(),
        format!("Bearer {}", source.api_key.clone().unwrap_or_default()),
    )]
}

fn one_day_range() -> (String, String) {
    use chrono::{Datelike, Local};
    let now = Local::now();
    let yest = now - chrono::Duration::days(1);
    let f = |d: &chrono::NaiveDate| format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day());
    (
        f(&yest.date_naive()),
        f(&now.date_naive()),
    )
}

fn pick(o: &Value, paths: &[&str]) -> Option<Value> {
    for p in paths {
        let mut cur = o;
        let mut found = true;
        for k in p.split('.') {
            match cur.get(k) {
                Some(v) => cur = v,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            return Some(cur.clone());
        }
    }
    None
}

fn num(v: Option<Value>) -> Option<f64> {
    v.as_ref()
        .and_then(Value::as_f64)
        .or_else(|| v.as_ref().and_then(Value::as_str).and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
}

/// 从响应推断额度（通用解析，容错）
pub fn infer_quota(json: &Value, _mode: &str) -> (Vec<QuotaItem>, bool) {
    let mut items: Vec<QuotaItem> = Vec::new();
    let data = match json.get("data") {
        Some(d) if d.is_object() => d.clone(),
        _ => json.clone(),
    };

    // 1. balance_infos（deepseek 风格）
    let infos = json
        .get("balance_infos")
        .or_else(|| data.get("balance_infos"))
        .and_then(Value::as_array);
    if let Some(infos) = infos {
        if !infos.is_empty() {
            for b in infos {
                let currency = b.get("currency").and_then(Value::as_str).unwrap_or("余额");
                let bal = num(b.get("total_balance").cloned());
                items.push(QuotaItem {
                    label: Some(currency.to_string()),
                    remaining: bal,
                    limit: bal,
                    used_pct: None,
                    currency: Some(currency.to_string()),
                    mode: Some("balance".to_string()),
                    ..Default::default()
                });
            }
            return (items, true);
        }
    }

    // 2. openrouter: data.usage / data.limit
    let usage = pick(&data, &["usage"]);
    let limit = pick(&data, &["limit"]);
    if usage.is_some() && limit.is_some() {
        let used = num(usage);
        let lim = num(limit);
        items.push(QuotaItem {
            label: Some("Key 额度".to_string()),
            remaining: match (lim, used) {
                (Some(l), Some(u)) => Some((l - u).max(0.0)),
                _ => None,
            },
            limit: lim,
            used_pct: match (lim, used) {
                (Some(l), Some(u)) if l > 0.0 => Some(((u / l) * 100.0).min(100.0)),
                _ => None,
            },
            currency: Some("USD".to_string()),
            mode: Some("openrouter".to_string()),
            ..Default::default()
        });
        return (items, true);
    }

    // 3. one-api: data.quota / data.used_quota（积分制）
    let quota = pick(&data, &["quota"]);
    let used_quota = pick(&data, &["used_quota"]);
    if quota.is_some() || used_quota.is_some() {
        let rem = num(quota);
        let used = num(used_quota);
        let lim = match (rem, used) {
            (Some(r), Some(u)) => Some(r + u),
            (Some(r), None) => Some(r),
            _ => None,
        };
        items.push(QuotaItem {
            label: Some("额度".to_string()),
            remaining: rem,
            limit: lim,
            used_pct: match (lim, used) {
                (Some(l), Some(u)) if l > 0.0 => Some(((u / l) * 100.0).min(100.0)),
                _ => None,
            },
            currency: Some("USD".to_string()),
            mode: Some("one-api".to_string()),
            note: Some("one-api 积分额度（非美元）".to_string()),
            ..Default::default()
        });
        return (items, true);
    }

    // 4. openai billing: total_usage（分）+ total_granted
    let total_usage = pick(json, &["total_usage", "data.total_usage"]);
    if let Some(tu) = total_usage {
        let used = num(Some(tu)).map(|v| v / 100.0);
        let granted = num(pick(json, &["total_granted", "data.total_granted"]));
        items.push(QuotaItem {
            label: Some("用量".to_string()),
            remaining: match (granted, used) {
                (Some(g), Some(u)) => Some((g - u).max(0.0)),
                _ => None,
            },
            limit: granted,
            used_pct: match (granted, used) {
                (Some(g), Some(u)) if g > 0.0 => Some(((u / g) * 100.0).min(100.0)),
                _ => None,
            },
            currency: Some("USD".to_string()),
            mode: Some("billing".to_string()),
            ..Default::default()
        });
        return (items, true);
    }

    (items, false)
}

fn build_candidate_urls(source: &SourceConfig) -> Vec<(String, String)> {
    let base = source.base_url.clone().unwrap_or_default();
    let base = base.trim_end_matches('/').to_string();
    let (start_date, end_date) = one_day_range();
    let mut urls = Vec::new();
    let bm = source.balance_mode.clone().unwrap_or_else(|| "auto".to_string());
    if bm == "custom" {
        if let Some(u) = &source.custom_balance_url {
            if !u.is_empty() {
                urls.push(("custom".to_string(), u.clone()));
            }
        }
    }
    if bm == "auto" || bm == "openai-billing" {
        urls.push((
            "openai-billing".to_string(),
            format!("{base}/v1/dashboard/billing/usage?start_date={start_date}&end_date={end_date}"),
        ));
    }
    if bm == "auto" || bm == "deepseek-balance" {
        urls.push(("deepseek-balance".to_string(), format!("{base}/v1/user/balance")));
        urls.push(("deepseek-balance".to_string(), format!("{base}/user/balance")));
    }
    if bm == "auto" || bm == "openrouter-key" {
        urls.push(("openrouter-key".to_string(), format!("{base}/api/v1/auth/key")));
    }
    if bm == "auto" || bm == "one-api" {
        urls.push(("one-api".to_string(), format!("{base}/api/user/self")));
    }
    urls
}

/// 探测并获取额度（auto 模式按候选顺序尝试）
pub async fn probe(source: &SourceConfig) -> serde_json::Value {
    let candidates = build_candidate_urls(source);
    if candidates.is_empty() {
        return serde_json::json!({ "ok": false, "error": "没有可探测的接口：请配置 Base URL 或自定义余额接口" });
    }
    let headers = auth_headers(source);
    let mut last_err: Option<String> = None;
    for (mode, url) in candidates {
        match get_json(&url, &headers, 20000).await {
            Ok(res) => {
                if res.status == 401 || res.status == 403 {
                    let body = res.body_json.clone().unwrap_or(Value::Null);
                    last_err = Some(format!("鉴权失败 ({mode}): {}", crate::util::err_msg(res.status, &body)));
                    continue;
                }
                if res.status >= 400 {
                    let body = res.body_json.clone().unwrap_or(Value::Null);
                    last_err = Some(format!("请求失败 ({mode}): {}", crate::util::err_msg(res.status, &body)));
                    continue;
                }
                let body = res.body_json.clone().unwrap_or_else(|| {
                    serde_json::from_str(&res.body_text).unwrap_or(Value::Null)
                });
                let (items, matched) = infer_quota(&body, &mode);
                if matched && !items.is_empty() {
                    return serde_json::json!({
                        "ok": true,
                        "mode": mode,
                        "matched": true,
                        "items": items,
                        "message": format!("探测成功：接口 {mode}，{} 项额度信息", items.len()),
                        "baseUrl": source.base_url.clone().unwrap_or_default(),
                    });
                }
                last_err = Some(format!("接口 {mode} 返回了数据但未能解析出额度字段"));
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }
    serde_json::json!({
        "ok": false,
        "mode": Value::Null,
        "matched": false,
        "error": last_err.unwrap_or_else(|| "所有接口均不可用".to_string()),
    })
}

/// 测试连接（设置界面用）
pub async fn test(source: &SourceConfig) -> serde_json::Value {
    let r = probe(source).await;
    if r.get("ok").and_then(Value::as_bool) == Some(true) {
        let items = r.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        let parts: Vec<String> = items
            .iter()
            .map(|i| {
                let label = i.get("label").and_then(Value::as_str).unwrap_or("?");
                let remaining = i.get("remaining").and_then(Value::as_f64);
                let currency = i.get("currency").and_then(Value::as_str).unwrap_or("");
                match remaining {
                    Some(v) => {
                        if currency == "USD" {
                            format!("{label}: ${v:.2}")
                        } else {
                            format!("{label}: {v} {currency}")
                        }
                    }
                    None => format!("{label}: ?"),
                }
            })
            .collect();
        let mut out = r.clone();
        let msg = r.get("message").and_then(Value::as_str).unwrap_or("");
        if !parts.is_empty() {
            out["message"] = Value::String(format!("{msg}（{}）", parts.join("；")));
        }
        out
    } else {
        r
    }
}
