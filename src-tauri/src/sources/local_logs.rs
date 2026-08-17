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
    } else if tool == "dsh" {
        // DeepSeek Harness：~/.dsh/sessions/<工作区>/session-<uuid>/session.jsonl.zstd
        ScanParams {
            recursive: true,
            pattern: Regex::new(r"^session\.jsonl\.zstd$").unwrap(),
            format: "dsh".to_string(),
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
/// 解析 ZCode 单条 model_io 记录 → RecordIn。
/// 统计口径与 zcode 界面一致：total = totalTokens（inputTokens 已含缓存命中 token，
/// 界面同样按含缓存累计）；input/output 存原始值。cache_read 保留供诊断/可选换算。
pub fn parse_zcode_record(r: &Value) -> Option<RecordIn> {
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
    let num = |k1: &str, k2: &str| {
        u.and_then(|x| x.get(k1).or_else(|| x.get(k2)))
            .and_then(Value::as_f64)
    };
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

// ---------- dsh（DeepSeek Harness）格式 ----------
// 会话日志 session.jsonl.zstd 是 zstd 压缩 JSONL，每行一个事件；只解析 assistant/message 的 usage。
// 注意：zstd 不支持按字节偏移增量读，调用方（watcher）采用「全量解压重读 + 已消费事件数去重」
//（文件只追加，新事件总在尾部），本函数只负责解压 + 提取记录，不含去重逻辑。

/// 解压后大小上限（防异常压缩流膨胀占满内存；正常会话解压后 ≤4MB）
const MAX_DSH_DECOMPRESSED: u64 = 256 * 1024 * 1024;

/// 解析单个 assistant/message 事件 → RecordIn；非目标事件/字段缺失返回 None（全容错）
fn parse_dsh_record(r: &Value) -> Option<RecordIn> {
    let data = r.get("data")?;
    let usage = data.get("usage")?;
    let input = usage.get("inputTokens").and_then(Value::as_f64).unwrap_or(0.0);
    let output = usage.get("outputTokens").and_then(Value::as_f64).unwrap_or(0.0);
    let cache_read = usage.get("cacheReadTokens").and_then(Value::as_f64).unwrap_or(0.0);
    // 口径与各客户端界面一致：total = input + output + cacheRead（缓存命中同样计入消耗量）。
    // dsh 每次请求都会重放整段会话缓存上下文（cacheRead 占大头），
    // 与 zcode 界面"含缓存 raw 累计"同口径
    let total = input + output + cache_read;
    if total <= 0.0 || !total.is_finite() {
        return None;
    }
    let src = data.pointer("/message/source");
    let model = src.and_then(|s| s.get("model")).and_then(Value::as_str).map(String::from);
    let provider = src.and_then(|s| s.get("provider")).and_then(Value::as_str).map(String::from);
    Some(RecordIn {
        ts: crate::util::parse_ts(&r["time"]),
        model,
        provider,
        input: Some(input),
        output: Some(output),
        total,
        cache_read: usage.get("cacheReadTokens").and_then(Value::as_f64),
        cost: None,
    })
}

/// 解压并解析 dsh 会话文件：zstd 解码 → 逐行 JSON → 仅取 assistant/message 的 usage。
/// 返回 None 表示**解压失败**（文件写入中/损坏/超限）——调用方必须不推进 seen/offset，
/// 下轮轮询自然重试；若返回空 Vec 则是"解压成功但无有效记录"，二者语义不同。
/// 不 panic、不影响其他源
pub fn parse_dsh_file(file_path: &Path) -> Option<Vec<RecordIn>> {
    let f = match std::fs::File::open(file_path) {
        Ok(f) => f,
        Err(_) => return None,
    };
    let mut text = String::new();
    let ok = (|| -> std::io::Result<bool> {
        use std::io::Read;
        let dec = zstd::stream::read::Decoder::new(f)?;
        let mut reader = dec.take(MAX_DSH_DECOMPRESSED + 1);
        let n = reader.read_to_string(&mut text)?;
        Ok(n <= MAX_DSH_DECOMPRESSED as usize)
    })();
    if !ok.unwrap_or(false) {
        return None;
    }
    let mut records = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let r: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // 写入中/损坏行跳过（解压整体成功，个别坏行忽略）
        };
        if r.get("type").and_then(Value::as_str) != Some("assistant/message") {
            continue;
        }
        if let Some(rec) = parse_dsh_record(&r) {
            records.push(rec);
        }
    }
    Some(records)
}

// ---------- Codex 格式（需按时间顺序全文件解析） ----------
// 返回的 u64 是每条记录所在行「内容末尾」的字节偏移（不含换行符）：
// watcher 用它做水位线去重——只累加偏移超出已消费 offset 的增量记录，
// 避免每次事件/轮询整文件重读导致 token 数 N 倍膨胀
pub fn parse_codex_file(file_path: &Path) -> (Vec<(RecordIn, u64)>, Vec<QuotaItem>) {
    let mut records: Vec<(RecordIn, u64)> = Vec::new();
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

    let mut line_off: u64 = 0;
    for line in text.split_inclusive('\n') {
        let line_len = line.len() as u64;
        // 内容长度去掉行尾换行符（\n / \r\n），偏移因此对两种换行都正确
        let content_end = line_off + line.trim_end_matches(['\n', '\r']).len() as u64;
        if line.trim().is_empty() {
            line_off += line_len;
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
                    records.push((
                        RecordIn {
                            ts: ts.or(Some(crate::util::now_ms())),
                            model: cur_model.clone().or_else(|| pick_model_from_record(&r)),
                            provider: Some("codex".to_string()),
                            input,
                            output,
                            total,
                            cache_read: u.get("cached_input_tokens").and_then(Value::as_f64),
                            cost: None,
                        },
                        content_end,
                    ));
                }
            }
        }
        if let Some(rl) = r.get("payload").and_then(|p| p.get("rate_limits")) {
            let ts_now = ts.unwrap_or_else(crate::util::now_ms);
            if let Some(item) = normalize_rate_limits(rl, ts_now) {
                rate_limits.push(item);
            }
        }
        line_off += line_len;
    }
    (records, rate_limits)
}

/// 通用入口：zcode / 自定义 → 逐行 zcode 解析；codex → 全文件有序解析；dsh → zstd 解压解析
pub fn parse_file(file_path: &Path, format: &str) -> (Vec<RecordIn>, Vec<QuotaItem>) {
    if format == "codex" {
        let (records, rate_limits) = parse_codex_file(file_path);
        return (records.into_iter().map(|(r, _)| r).collect(), rate_limits);
    }
    if format == "dsh" {
        return (parse_dsh_file(file_path).unwrap_or_default(), Vec::new());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn token_count_line(ts: &str, total: i64) -> String {
        serde_json::json!({
            "type": "event_msg",
            "timestamp": ts,
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        // 真实 codex 记录：total_tokens = input_tokens + output_tokens（无缓存时）
                        "input_tokens": total - 20,
                        "output_tokens": 20,
                        "total_tokens": total
                    }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn codex_watermark_filters_accumulated_history() {
        let dir = std::env::temp_dir().join(format!(
            "usage-test-codex-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout-test.jsonl");
        let mut f = std::fs::File::create(&file).unwrap();
        // 第一段：两条记录 → 水位线 = 文件大小
        writeln!(f, "{}", token_count_line("2026-08-12T01:00:00Z", 100)).unwrap();
        writeln!(f, "{}", token_count_line("2026-08-12T01:01:00Z", 200)).unwrap();
        f.flush().unwrap();
        let size1 = std::fs::metadata(&file).unwrap().len();

        let (records, _) = parse_codex_file(&file);
        assert_eq!(records.len(), 2, "首轮应解析出全部 2 条");
        let all_off: Vec<u64> = records.iter().map(|(_, o)| *o).collect();
        assert!(all_off[0] < all_off[1] && all_off[1] <= size1, "偏移应单调且不越过文件");

        // 模拟文件增长：再追加两条记录
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(f, "{}", token_count_line("2026-08-12T01:02:00Z", 300)).unwrap();
        writeln!(f, "{}", token_count_line("2026-08-12T01:03:00Z", 400)).unwrap();
        f.flush().unwrap();

        let (records, _) = parse_codex_file(&file);
        assert_eq!(records.len(), 4, "整文件解析出全部 4 条");
        // 水位线去重：只保留内容末尾偏移超出 size1 的增量记录
        let delta: Vec<&RecordIn> = records
            .iter()
            .filter(|(_, end_off)| *end_off > size1)
            .map(|(r, _)| r)
            .collect();
        assert_eq!(delta.len(), 2, "水位线之后应只有 2 条新增记录");
        assert_eq!(delta.iter().map(|r| r.total as i64).sum::<i64>(), 700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zcode_parse_counts_raw_total_including_cache() {
        // 真实样本结构：totalTokens 含缓存命中，与 zcode 界面口径一致（含缓存累计）
        let line = serde_json::json!({
            "type": "model_io",
            "completedAt": "2026-08-17T05:35:12.903Z",
            "model": {"modelId": "deepseek-v4-flash", "providerId": "p1", "role": "lite"},
            "response": {"usage": {
                "inputTokens": 32298.0, "outputTokens": 630.0,
                "totalTokens": 32928.0, "cacheReadTokens": 30720.0, "cacheWriteTokens": 0.0
            }}
        });
        let rec = parse_zcode_record(&line).expect("应解析出记录");
        // 与 zcode 界面一致：total = totalTokens（含缓存），input/output 存原始值
        assert_eq!(rec.total, 32928.0);
        assert_eq!(rec.input, Some(32298.0));
        assert_eq!(rec.output, Some(630.0));
        assert_eq!(rec.cache_read, Some(30720.0));
        assert_eq!(rec.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(rec.ts, Some(1786944912903));

        // 纯缓存命中请求：totalTokens > 0 即计入（与界面一致）
        let cache_only = serde_json::json!({
            "type": "model_io",
            "completedAt": "2026-08-17T06:00:00.000Z",
            "model": {"modelId": "deepseek-v4-flash"},
            "response": {"usage": {
                "inputTokens": 512.0, "outputTokens": 0.0,
                "totalTokens": 512.0, "cacheReadTokens": 512.0, "cacheWriteTokens": 0.0
            }}
        });
        let rec3 = parse_zcode_record(&cache_only).expect("纯缓存命中也计入");
        assert_eq!(rec3.total, 512.0);

        // 缺 input/output 字段：total 用 totalTokens
        let total_only = serde_json::json!({
            "type": "model_io",
            "completedAt": "2026-08-17T06:01:00.000Z",
            "model": {"modelId": "m"},
            "response": {"usage": {"totalTokens": 1000.0, "cacheReadTokens": 400.0}}
        });
        let rec2 = parse_zcode_record(&total_only).expect("total-only 应解析出记录");
        assert_eq!(rec2.total, 1000.0);
        assert_eq!(rec2.input, None);
    }

    #[test]
    fn dsh_parses_assistant_message_only() {
        let dir = std::env::temp_dir().join(format!(
            "usage-test-dsh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("session.jsonl.zstd");
        {
            let f = std::fs::File::create(&file).unwrap();
            let mut enc = zstd::stream::write::Encoder::new(f, 0).unwrap();
            // assistant/message：完整 usage
            let msg = serde_json::json!({
                "type": "assistant/message",
                "seq": 1,
                "time": 1754793600000i64,
                "data": {
                    "message": {
                        "source": { "kind": "model", "provider": "opencode-go", "model": "deepseek-v4-flash" }
                    },
                    "usage": { "inputTokens": 8057, "outputTokens": 211, "cacheReadTokens": 8192 }
                }
            });
            writeln!(enc, "{}", msg).unwrap();
            // 非 assistant/message 事件（chunk 类）应被跳过
            let chunk = serde_json::json!({
                "type": "reasoning-chunks",
                "data": { "usage": { "inputTokens": 1, "outputTokens": 2 } }
            });
            writeln!(enc, "{}", chunk).unwrap();
            // usage 缺失的 assistant/message 也应跳过
            let no_usage = serde_json::json!({ "type": "assistant/message", "time": 1754793600001i64, "data": { "message": {} } });
            writeln!(enc, "{}", no_usage).unwrap();
            // 纯缓存命中请求（input+output=0 但 cacheRead>0）：按含缓存口径同样计入 total
            let cache_only = serde_json::json!({
                "type": "assistant/message",
                "time": 1754793600002i64,
                "data": {
                    "message": { "source": { "kind": "model", "provider": "opencode-go", "model": "deepseek-v4-flash" } },
                    "usage": { "inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 1000 }
                }
            });
            writeln!(enc, "{}", cache_only).unwrap();
            enc.finish().unwrap();
        }
        let records = parse_dsh_file(&file).unwrap();
        assert_eq!(records.len(), 2, "应解析出 2 条：完整 usage + 纯缓存命中（均按含缓存口径计入）");
        let r = &records[0];
        assert_eq!(r.ts, Some(1754793600000));
        assert_eq!(r.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(r.provider.as_deref(), Some("opencode-go"));
        assert_eq!(r.input, Some(8057.0));
        assert_eq!(r.output, Some(211.0));
        // 与各客户端界面一致：total = input + output + cacheRead（缓存计入消耗量）
        assert_eq!(r.total, 8057.0 + 211.0 + 8192.0);
        assert_eq!(r.cache_read, Some(8192.0));
        assert_eq!(r.cost, None);
        let r2 = &records[1];
        assert_eq!(r2.total, 1000.0, "纯缓存命中也计入 total");
        assert_eq!(r2.cache_read, Some(1000.0));

        // 损坏文件（非 zstd）→ None（解压失败），不 panic
        std::fs::write(&file, b"not a zstd stream").unwrap();
        assert!(parse_dsh_file(&file).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
