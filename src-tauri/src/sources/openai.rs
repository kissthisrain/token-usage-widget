// OpenAI 官方用量 API 适配器（对齐 core/sources/openai.js）

use crate::config::SourceConfig;
use crate::history::RecordIn;
use crate::http::get_json;
use crate::util::{date_key, parse_ts};
use serde_json::Value;

fn build_urls(base: &str, start: i64, end: i64) -> Vec<String> {
    let q = format!("start_time={start}&end_time={end}&bucket_width=1d&group_by[]=model");
    vec![
        format!("{base}/v1/usage?{q}"),
        format!("{base}/v1/organization/usage/completions?{q}"),
    ]
}

fn auth_headers(source: &SourceConfig) -> Vec<(String, String)> {
    let mut h = vec![(
        "Authorization".to_string(),
        format!("Bearer {}", source.api_key.clone().unwrap_or_default()),
    )];
    if let Some(org) = &source.organization_id {
        if !org.is_empty() {
            h.push(("OpenAI-Organization".to_string(), org.clone()));
        }
    }
    h
}

/// 解析两种接口的响应 → 归一化记录列表
pub fn parse_usage_payload(payload: &serde_json::Value) -> Vec<RecordIn> {
    let mut records = Vec::new();
    let buckets = payload.get("data").and_then(value_array).cloned().unwrap_or_default();
    for bucket in &buckets {
        let ts = parse_ts(&bucket["start_time"]).or_else(|| parse_ts(&bucket["startTime"]));
        let Some(ts) = ts else { continue };
        let results = bucket.get("results").and_then(value_array).cloned().unwrap_or_default();
        for r in &results {
            let input = pick_num(r, &["input_tokens", "inputTokens"]);
            let output = pick_num(r, &["output_tokens", "outputTokens"]);
            let total = pick_num(r, &["total_tokens", "totalTokens"])
                .or(match (input, output) {
                    (Some(i), Some(o)) => Some(i + o),
                    _ => None,
                });
            let Some(total) = total else { continue };
            let cost_raw = r.get("cost").cloned();
            let cost = match &cost_raw {
                Some(Value::Object(o)) => o
                    .get("amount")
                    .and_then(Value::as_f64)
                    .filter(|c| c.is_finite()),
                Some(v) => v.as_f64().filter(|c| c.is_finite()),
                None => None,
            };
            let model = pick_str(r, &["model", "api_model", "model_id"]);
            records.push(RecordIn {
                ts: Some(ts),
                model,
                provider: None,
                input,
                output,
                total,
                cache_read: None,
                cost,
                credits: None,
            });
        }
    }
    records
}

/// 拉取最近 N 天用量（对齐 fetchUsage：新接口 404 时试旧接口；403 特化提示）
pub async fn fetch_usage(source: &SourceConfig, days: i64) -> Result<Vec<RecordIn>, String> {
    let base = source
        .base_url
        .clone()
        .unwrap_or_else(|| "https://api.openai.com".to_string());
    let base = base.trim_end_matches('/').to_string();
    let end = crate::util::now_ms() / 1000;
    let start = end - days * 86400;
    let urls = build_urls(&base, start, end);
    let headers = auth_headers(source);
    let mut last_err: Option<String> = None;
    for url in urls {
        match get_json(&url, &headers, 20000).await {
            Ok(res) => {
                if (200..300).contains(&res.status) {
                    let body = res.body_json.clone().unwrap_or(serde_json::Value::Null);
                    return Ok(parse_usage_payload(&body));
                }
                if res.status == 403 {
                    return Err("用量接口返回 403：需要「组织管理员」API Key（在 platform.openai.com → Organization → API keys 创建），普通 Key 无用量权限".to_string());
                }
                let body = res.body_json.clone().unwrap_or(serde_json::Value::Null);
                last_err = Some(crate::util::err_msg(res.status, &body));
                if res.status != 404 {
                    break;
                }
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "OpenAI 用量接口请求失败".to_string()))
}

/// 测试连接
pub async fn test(source: &SourceConfig) -> serde_json::Value {
    match fetch_usage(source, 3).await {
        Ok(records) => {
            let days_set: std::collections::HashSet<String> =
                records.iter().filter_map(|r| r.ts.map(date_key)).collect();
            let mut days: Vec<String> = days_set.into_iter().collect();
            days.sort();
            let days = days[days.len().saturating_sub(3)..].to_vec();
            serde_json::json!({
                "ok": true,
                "message": format!("连接成功：3 天内 {} 条模型用量记录，覆盖 {} 天", records.len(), days.len()),
                "records": records.len(),
                "days": days,
            })
        }
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

fn value_array(v: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    v.as_array()
}

fn pick_str(r: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = r.get(*k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn pick_num(r: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = r.get(*k).and_then(Value::as_f64) {
            return Some(n);
        }
    }
    None
}
