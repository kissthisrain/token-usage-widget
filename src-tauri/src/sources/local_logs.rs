// 本地 JSONL 记录解析：ZCode（camelCase）与 Codex（snake_case + 事件结构）
// 对齐 core/sources/local-logs.js

use crate::config::SourceConfig;
use crate::history::{CreditsInfo, QuotaItem, RecordIn};
use crate::util::parse_ts;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct ScanParams {
    pub recursive: bool,
    pub pattern: Regex,
    pub format: String,
}

/// 源配置 → 扫描参数
pub fn scan_params_for_source(src: &SourceConfig) -> ScanParams {
    let tool = src.tool.as_deref().unwrap_or("zcode");
    if tool == "codex" {
        ScanParams {
            recursive: true,
            pattern: Regex::new(r"^rollout-.*\.jsonl$").unwrap(),
            format: "codex".to_string(),
        }
    } else if tool == "zcode" {
        ScanParams {
            recursive: false,
            pattern: Regex::new(r"^model-io-.*\.jsonl$").unwrap(),
            format: "zcode".to_string(),
        }
    } else {
        let pat = src.file_pattern.clone().unwrap_or_else(|| r"\.jsonl$".to_string());
        ScanParams {
            recursive: src.recursive.unwrap_or(false),
            pattern: Regex::new(&pat).unwrap_or_else(|_| Regex::new(r"\.jsonl$").unwrap()),
            format: src.format.clone().unwrap_or_else(|| "zcode".to_string()),
        }
    }
}

// ---------- ZCode 格式 ----------
fn parse_zcode_record(r: &Value) -> Option<RecordIn> {
    if !r.is_object() {
        return None;
    }
    let u = r.get("response").and_then(|x| x.get("usage"));
    let total = u
        .and_then(|x| x.get("totalTokens").or_else(|| x.get("total_tokens")))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if total <= 0.0 || !total.is_finite() {
        return None;
    }
    let ts = parse_ts(&r["completedAt"])
        .or_else(|| parse_ts(&r["timestamp"]))
        .or_else(|| parse_ts(&r["startedAt"]));
    let model = r
        .get("model")
        .and_then(|m| m.get("modelId").or_else(|| m.get("id")))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .or_else(|| r.get("model").and_then(Value::as_str).map(|s| s.to_string()));
    let provider = r
        .get("model")
        .and_then(|m| m.get("providerId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let num = |k1: &str, k2: &str| u.and_then(|x| x.get(k1).or_else(|| x.get(k2))).and_then(Value::as_f64);
    Some(RecordIn {
        ts,
        model,
        provider,
        input: num("inputTokens", "input_tokens"),
        output: num("outputTokens", "output_tokens"),
        total,
        cache_read: num("cacheReadTokens", "cached_input_tokens"),
        cost: None,
    })
}

fn pick_model_from_record(r: &Value) -> Option<String> {
    let s = serde_json::to_string(r).unwrap_or_default();
    let re = Regex::new(r#""model"\s*:\s*"([^"]+)""#).unwrap();
    re.captures(&s).map(|c| c[1].to_string())
}

fn normalize_rate_limits(rl: &Value, ts: i64) -> Option<QuotaItem> {
    if !rl.is_object() {
        return None;
    }
    let id = rl.get("limit_id").and_then(Value::as_str)?;
    let p = rl.get("primary").cloned().unwrap_or(Value::Null);
    let credits = if rl.get("credits").is_some() && rl["credits"].is_object() {
        let c = &rl["credits"];
        Some(CreditsInfo {
            has_credits: c.get("has_credits").and_then(Value::as_bool).unwrap_or(false),
            unlimited: c.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
            balance: c
                .get("balance")
                .filter(|b| !b.is_null())
                .and_then(Value::as_f64),
        })
    } else {
        None
    };
    let pct = p.get("used_percent").and_then(Value::as_f64);
    let win = p.get("window_minutes").and_then(Value::as_f64).map(|v| v as i64);
    let resets = p
        .get("resets_at")
        .and_then(Value::as_f64)
        .map(|v| (v * 1000.0) as i64);
    let has_real = pct.is_some()
        || win.is_some()
        || resets.is_some()
        || credits.as_ref().is_some_and(|c| {
            c.balance.is_some() || c.has_credits || c.unlimited
        });
    if !has_real {
        return None;
    }
    Some(QuotaItem {
        limit_id: Some(id.to_string()),
        label: Some(
            rl.get("limit_name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string(),
        ),
        used_pct: pct,
        window_minutes: win,
        resets_at: resets,
        credits,
        plan_type: rl.get("plan_type").and_then(Value::as_str).map(|s| s.to_string()),
        at: Some(ts),
        ..Default::default()
    })
}

// ---------- Codex 格式（需按时间顺序全文件解析） ----------
pub fn parse_codex_file(file_path: &Path) -> (Vec<RecordIn>, Vec<QuotaItem>) {
    let mut records: Vec<RecordIn> = Vec::new();
    let mut rate_limits: Vec<QuotaItem> = Vec::new();
    let mut cur_model: Option<String> = None;
    let mut last_sig: Option<String> = None;

    let text = match std::fs::read_to_string(file_path) {
        Ok(t) => t,
        Err(_) => return (records, rate_limits),
    };
    if text.trim().is_empty() {
        return (records, rate_limits);
    }

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let r: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if r.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(m) = r.get("payload").and_then(|p| p.get("model")).and_then(Value::as_str) {
                cur_model = Some(m.to_string());
            }
            continue;
        }
        let is_event_token_count = r.get("type").and_then(Value::as_str) == Some("event_msg")
            && r.get("payload").and_then(|p| p.get("type")).and_then(Value::as_str) == Some("token_count");
        if !is_event_token_count {
            continue;
        }
        let ts = parse_ts(&r["timestamp"]);
        let info = r.get("payload").and_then(|p| p.get("info")).cloned().unwrap_or(Value::Null);
        let u = info
            .get("last_token_usage")
            .or_else(|| info.get("total_token_usage"))
            .cloned();
        if let Some(u) = u {
            let total = u.get("total_tokens").and_then(Value::as_f64).unwrap_or(0.0);
            if total > 0.0 && total.is_finite() {
                let input = u.get("input_tokens").and_then(Value::as_f64);
                let output = u.get("output_tokens").and_then(Value::as_f64);
                let sig = format!("{ts:?}|{cur_model:?}|{input:?}|{output:?}|{total}");
                if last_sig.as_deref() != Some(sig.as_str()) {
                    last_sig = Some(sig);
                    records.push(RecordIn {
                        ts: ts.or(Some(crate::util::now_ms())),
                        model: cur_model.clone().or_else(|| pick_model_from_record(&r)),
                        provider: Some("codex".to_string()),
                        input,
                        output,
                        total,
                        cache_read: u.get("cached_input_tokens").and_then(Value::as_f64),
                        cost: None,
                    });
                }
            }
        }
        if let Some(rl) = r.get("payload").and_then(|p| p.get("rate_limits")) {
            let ts_now = ts.unwrap_or_else(crate::util::now_ms);
            if let Some(item) = normalize_rate_limits(rl, ts_now) {
                rate_limits.push(item);
            }
        }
    }
    (records, rate_limits)
}

/// 通用入口：zcode / 自定义 → 逐行 zcode 解析；codex → 全文件有序解析
pub fn parse_file(file_path: &Path, format: &str) -> (Vec<RecordIn>, Vec<QuotaItem>) {
    if format == "codex" {
        return parse_codex_file(file_path);
    }
    let text = match std::fs::read_to_string(file_path) {
        Ok(t) => t,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let mut records = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let r: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(rec) = parse_zcode_record(&r) {
            records.push(rec);
        }
    }
    (records, Vec::new())
}

/// 目录扫描：非递归（zcode rollout）或递归（codex sessions/自定义）；返回排序后的文件列表
pub fn list_jsonl_files(dir: &Path, recursive: bool, pattern: &Regex) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, recursive: bool, pattern: &Regex, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for ent in entries.flatten() {
            let p = ent.path();
            if ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if recursive {
                    walk(&p, recursive, pattern, out);
                }
            } else if let Some(name) = ent.file_name().to_str() {
                if pattern.is_match(name) {
                    out.push(p);
                }
            }
        }
    }
    walk(dir, recursive, pattern, &mut out);
    out.sort();
    out
}

/// 多行安全 JSON 记录提取：从 pos 开始找 '{'，扫描配对的 '}'（容忍记录内换行/转义）
/// 返回 (start, end)；未找到完整记录（写入中/损坏）返回 None
pub fn extract_json_record(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < buf.len() && buf[i] != 0x7b {
        /* '{' */
        i += 1;
    }
    if i >= buf.len() {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    #[allow(clippy::needless_range_loop)] // 字节扫描需按位置索引
    for j in i..buf.len() {
        let c = buf[j];
        if in_str {
            if esc {
                esc = false;
            } else if c == 0x5c {
                esc = true;
            } else if c == 0x22 {
                in_str = false;
            }
            continue;
        }
        if c == 0x22 {
            in_str = true;
        } else if c == 0x7b {
            depth += 1;
        } else if c == 0x7d {
            depth -= 1;
            if depth == 0 {
                return Some((i, j + 1));
            }
        }
    }
    None // 未闭合（写入中）
}
