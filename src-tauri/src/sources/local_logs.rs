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
        // 以及新版 session.v4.jsonl.zstd（后续 session.vN.jsonl.zstd 同样匹配）
        ScanParams {
            recursive: true,
            pattern: Regex::new(r"^session(?:\.v\d+)?\.jsonl\.zstd$").unwrap(),
            format: "dsh".to_string(),
        }
    } else if tool == "workbuddy" {
        // WorkBuddy（两版共用本解析器）：
        //   国际版 ~/.workbuddy-ai/projects/<项目slug>/<会话uuid>.jsonl
        //   国内版 ~/.workbuddy/projects/<项目slug>/<会话uuid>.jsonl（追加式，结构一致）
        // 同目录的 <uuid>.file-rollback.ndjson 扩展名为 .ndjson，不匹配 `\.jsonl$`，天然排除。
        ScanParams {
            recursive: true,
            pattern: Regex::new(r"\.jsonl$").unwrap(),
            format: "workbuddy".to_string(),
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
        credits: None,
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
// 会话日志 session.jsonl.zstd / session.vN.jsonl.zstd 是 zstd 压缩 JSONL，每行一个事件；只解析 assistant/message 的 usage。
// 注意：zstd 不支持按字节偏移增量读，调用方（watcher）采用「全量解压重读 + 已消费事件数去重」
//（文件只追加，新事件总在尾部），本函数只负责解压 + 提取记录（含 seeded 会话的继承段过滤），不含去重逻辑。
//
// seeded 会话（子代理 / sidechat fork）的文件开头是**继承来的父会话历史**，其 assistant/message
// 与原会话完全同构（同 time / 同 usage），逐条累加会把父会话的用量再算一遍（实测 10-08 多计 7.5M）。
// 边界见 DshSeedFilter。

/// 解压后大小上限（防异常压缩流膨胀占满内存；正常会话解压后 ≤4MB）
const MAX_DSH_DECOMPRESSED: u64 = 256 * 1024 * 1024;

/// dsh seeded 会话的继承段过滤规则。
///
/// 官方语义（app.asar 内 v2/v3/v4 三套校验器一致）：
/// - `header.isSeeded === true` 表示该文件是 fork/子代理会话，开头若干事件是继承的父会话历史；
/// - `session/end-seed` 的 `data.inherited === true` 是**继承段收尾标记**，官方把它所在 seq 记为
///   `inheritedEventCount`，并强制「有且只有最后一条」（`lastInheritedMarker === cut`）；
///   即：该行及其之前的全部事件都不属于本会话；
/// - 老格式（v0/v2，header 无 isSeeded）不含该标记，规则自动退化为 no-op，历史数据不受影响。
///
/// 退化判据：有 isSeeded 但找不到标记时，用 `header.createdAt` —— 子会话 createdAt 之前
/// 不可能产生它自己的调用。实测两种判据在本机 12 个会话文件上结果完全一致。
#[derive(Default)]
struct DshSeedFilter {
    /// 最后一条 `session/end-seed`（`data.inherited == true`）的行号；该行及之前整段丢弃
    inherited_through: Option<usize>,
    /// header.createdAt（毫秒），无 inherited 标记时的退化判据
    created_at: Option<i64>,
}

impl DshSeedFilter {
    /// 从解压后的文本里提取过滤规则（只读首行 header + 扫一遍 end-seed 标记）
    fn from_text(text: &str) -> Self {
        let mut f = DshSeedFilter::default();
        // header = 第一行非空行；只有 isSeeded == true 的会话才启用过滤
        let header_seeded = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("session"))
            .map(|v| {
                let seeded = v.get("isSeeded").and_then(Value::as_bool).unwrap_or(false);
                if seeded {
                    f.created_at = crate::util::parse_ts(&v["createdAt"]);
                }
                seeded
            })
            .unwrap_or(false);
        for (idx, line) in text.lines().enumerate() {
            // 快速预筛，避免对每行做 JSON 解析
            if !line.contains("session/end-seed") {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if v.get("type").and_then(Value::as_str) == Some("session/end-seed")
                    && v.pointer("/data/inherited").and_then(Value::as_bool) == Some(true)
                {
                    f.inherited_through = Some(idx);
                }
            }
        }
        if !header_seeded {
            // 非 seeded：即便出现标记也按「无继承」处理（官方校验器视其为格式非法），
            // 这样旧格式数据的行为与修复前完全一致
            f.inherited_through = None;
            f.created_at = None;
        }
        f
    }

    /// 该行是否为继承内容（应丢弃）
    fn skips(&self, line_index: usize, ts: Option<i64>) -> bool {
        if let Some(cut) = self.inherited_through {
            return line_index <= cut;
        }
        match self.created_at {
            // ts 缺失时保守保留（宁可多算一条，也不丢真实用量）
            Some(c) => ts.is_some_and(|t| t < c),
            None => false,
        }
    }

    fn is_noop(&self) -> bool {
        self.inherited_through.is_none() && self.created_at.is_none()
    }
}

/// 解析单个 assistant/message 事件 → RecordIn；非目标事件/字段缺失返回 None（全容错）
fn parse_dsh_record(r: &Value) -> Option<RecordIn> {
    let data = r.get("data")?;
    let usage = data.get("usage")?;
    let input = usage.get("inputTokens").and_then(Value::as_f64).unwrap_or(0.0);
    let output = usage.get("outputTokens").and_then(Value::as_f64).unwrap_or(0.0);
    let cache_read = usage.get("cacheReadTokens").and_then(Value::as_f64).unwrap_or(0.0);
    let cache_write = usage.get("cacheWriteTokens").and_then(Value::as_f64).unwrap_or(0.0);
    // 口径与 dsh 官方 usageTokens() 一致：total = input + output + cacheRead + cacheWrite
    //（四个互不重叠的桶；缓存命中同样计入消耗量）。
    // dsh 每次请求都会重放整段会话缓存上下文（cacheRead 占大头），
    // 与 zcode 界面"含缓存 raw 累计"同口径
    let total = input + output + cache_read + cache_write;
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
        credits: None,
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
    // seeded 会话（子代理 fork）需要跳过开头的父会话继承段；
    // 非 seeded / 老格式（无 isSeeded 字段）→ filter 为空，行为与修复前完全一致
    let filter = DshSeedFilter::from_text(&text);
    let mut records = Vec::new();
    for (idx, line) in text.lines().enumerate() {
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
        if !filter.is_noop() && filter.skips(idx, crate::util::parse_ts(&r["time"])) {
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
                            credits: None,
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

// ---------- WorkBuddy（本 agent）格式 ----------
/// 解析 WorkBuddy 单条会话记录 → RecordIn。
///
/// 用量挂在 `providerData.rawUsage`（OpenAI 风格）：
/// `prompt_tokens` 已含缓存命中（`prompt_cache_hit_tokens`），因此 total 与 zcode 界面口径一致
/// （含缓存累计）。同一会话里每条带 rawUsage 的记录对应一次模型请求，`providerData.messageId`
/// 唯一，配合文件 offset 增量读取即可天然去重。
/// 注意：`prompt_tokens` 是每轮的完整上下文，agent 多轮工具循环会重复计入（行业标准口径）。
pub fn parse_workbuddy_record(r: &Value) -> Option<RecordIn> {
    if !r.is_object() {
        return None;
    }
    let pd = r.get("providerData")?;
    let u = pd.get("rawUsage")?;
    let total = u
        .get("total_tokens")
        .or_else(|| u.get("totalTokens"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if total <= 0.0 || !total.is_finite() {
        return None;
    }
    let ts = parse_ts(&r["timestamp"]).or_else(|| parse_ts(&r["completedAt"]));
    let model = pd
        .get("requestModelName")
        .or_else(|| pd.get("model"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let num = |k: &str| u.get(k).and_then(Value::as_f64);
    // rawUsage.credit 是「积分」而非美元 → 记入 credits（不写 cost），
    // 避免积分被当成美元混进费用合计；credit 为 0 表示免费额度/未计价，不记。
    let credits = num("credit").filter(|c| *c > 0.0);
    Some(RecordIn {
        ts,
        model,
        provider: None,
        input: num("prompt_tokens"),
        output: num("completion_tokens"),
        total,
        cache_read: num("prompt_cache_hit_tokens").or_else(|| num("cache_read_input_tokens")),
        cost: None,
        credits,
    })
}

/// 通用入口：zcode / 自定义 / workbuddy → 逐行解析；codex → 全文件有序解析；dsh → zstd 解压解析
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
        let parsed = if format == "workbuddy" {
            parse_workbuddy_record(&r)
        } else {
            parse_zcode_record(&r)
        };
        if let Some(rec) = parsed {
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
    fn workbuddy_parse_reads_provider_data_raw_usage() {
        // 真实样本结构：用量挂在 providerData.rawUsage（OpenAI 风格），
        // prompt_tokens 已含缓存命中；model 取 requestModelName，ts 取 record.timestamp
        let line = serde_json::json!({
            "type": "function_call",
            "timestamp": 1791872008668i64,
            "providerData": {
                "model": "deepseek-v4.1-flash",
                "requestModelName": "Deepseek-V4.1-Flash",
                "messageId": "01a133e4ebee7b02b46a2d41f863e617",
                "rawUsage": {
                    "prompt_tokens": 34392, "completion_tokens": 350, "total_tokens": 34742,
                    "prompt_cache_hit_tokens": 0, "prompt_cache_miss_tokens": 34392,
                    "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "credit": 0
                }
            }
        });
        let rec = parse_workbuddy_record(&line).expect("应解析出记录");
        assert_eq!(rec.total, 34742.0);
        assert_eq!(rec.input, Some(34392.0));
        assert_eq!(rec.output, Some(350.0));
        assert_eq!(rec.cache_read, Some(0.0));
        assert_eq!(rec.model.as_deref(), Some("Deepseek-V4.1-Flash"));
        assert_eq!(rec.ts, Some(1791872008668));
        assert_eq!(rec.cost, None, "workbuddy 不写美元费用");
        assert_eq!(rec.credits, None, "credit=0 不记积分");

        // 无 rawUsage 的记录（message/reasoning 等）不产生用量
        let no_usage = serde_json::json!({ "type": "reasoning", "timestamp": 1i64, "providerData": {} });
        assert!(parse_workbuddy_record(&no_usage).is_none());

        // credit>0 时记入「积分」（不是美元费用）
        let paid = serde_json::json!({
            "timestamp": 1791872008668i64,
            "providerData": { "rawUsage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15, "credit": 0.02 } }
        });
        let rec2 = parse_workbuddy_record(&paid).expect("应解析出记录");
        assert_eq!(rec2.total, 15.0);
        assert_eq!(rec2.cost, None, "积分不能写成美元费用");
        assert_eq!(rec2.credits, Some(0.02), "credit 应记入积分");
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
    fn dsh_scan_matches_legacy_and_versioned_session_files() {
        let params = scan_params_for_source(&SourceConfig {
            tool: Some("dsh".to_string()),
            ..Default::default()
        });
        assert!(params.pattern.is_match("session.jsonl.zstd"));
        assert!(params.pattern.is_match("session.v4.jsonl.zstd"));
        assert!(params.pattern.is_match("session.v5.jsonl.zstd"));
        assert!(!params.pattern.is_match("session.jsonl"));
        assert!(!params.pattern.is_match("other.jsonl.zstd"));
        assert!(!params.pattern.is_match("session.v4.jsonl"));
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
        // 与 dsh 官方 usageTokens() 一致：total = input + output + cacheRead + cacheWrite（缓存计入消耗量）
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

    /// seeded 子代理会话（sidechat fork）文件开头是继承的父会话历史，必须整段跳过，
    /// 否则父会话的 usage 会被子会话文件再累加一遍（实测 2026-10-08 因此多计 7.5M tokens）。
    #[test]
    fn dsh_seeded_session_skips_inherited_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "usage-test-dsh-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let created_at: i64 = 1754793600000;
        let msg = |ts: i64, input: f64| {
            serde_json::json!({
                "type": "assistant/message",
                "time": ts,
                "data": {
                    "message": { "source": { "kind": "model", "provider": "deepseek-account", "model": "deepseek-flash" } },
                    "usage": { "inputTokens": input, "outputTokens": 0, "cacheReadTokens": 0 }
                }
            })
        };
        let end_seed = |inherited: bool| {
            let data = if inherited {
                serde_json::json!({ "inherited": true })
            } else {
                serde_json::json!({})
            };
            serde_json::json!({ "type": "session/end-seed", "time": created_at, "data": data })
        };

        // --- seeded 会话：header isSeeded=true + 继承段（2 条 assistant/message）+ 最后一条 inherited 标记 + 自己 1 条 ---
        let file = dir.join("session.v4.jsonl.zstd");
        {
            let f = std::fs::File::create(&file).unwrap();
            let mut enc = zstd::stream::write::Encoder::new(f, 0).unwrap();
            let header = serde_json::json!({
                "type": "session",
                "version": 4,
                "id": "session-child",
                "createdAt": created_at,
                "isSeeded": true,
                "origin": "subagent",
                "delegationDepth": 1,
                "parentSession": "session-parent"
            });
            writeln!(enc, "{}", header).unwrap();
            // 继承自重放：time 早于子会话 createdAt
            writeln!(enc, "{}", msg(created_at - 60000, 1000.0)).unwrap();
            writeln!(enc, "{}", msg(created_at - 30000, 2000.0)).unwrap();
            // 分段标记（data.inherited 缺省 = 普通种子结束，不是继承段终点）
            writeln!(enc, "{}", end_seed(false)).unwrap();
            writeln!(enc, "{}", msg(created_at - 10000, 4000.0)).unwrap();
            // 继承段收尾标记：只认 `inherited: true`，此前的全部内容都要丢弃
            writeln!(enc, "{}", end_seed(true)).unwrap();
            // 子会话自己的调用
            writeln!(enc, "{}", msg(created_at + 5000, 8192.0)).unwrap();
            enc.finish().unwrap();
        }
        let records = parse_dsh_file(&file).unwrap();
        assert_eq!(records.len(), 1, "seeded 会话只应保留继承段之后的记录");
        assert_eq!(records[0].ts, Some(created_at + 5000));
        assert_eq!(records[0].total, 8192.0);

        // --- 非 seeded 会话：即便出现 end-seed 标记也不过虑（老格式无 isSeeded，行为必须与修复前一致）---
        let plain = dir.join("session.jsonl.zstd");
        {
            let f = std::fs::File::create(&plain).unwrap();
            let mut enc = zstd::stream::write::Encoder::new(f, 0).unwrap();
            let header = serde_json::json!({
                "type": "session",
                "version": 4,
                "id": "session-plain",
                "createdAt": created_at,
                "isSeeded": false
            });
            writeln!(enc, "{}", header).unwrap();
            writeln!(enc, "{}", msg(created_at - 60000, 1000.0)).unwrap();
            writeln!(enc, "{}", end_seed(true)).unwrap();
            writeln!(enc, "{}", msg(created_at + 5000, 8192.0)).unwrap();
            enc.finish().unwrap();
        }
        let records = parse_dsh_file(&plain).unwrap();
        assert_eq!(records.len(), 2, "非 seeded 会话不做任何过滤");

        // --- 退化路径：isSeeded=true 但没有 inherited 标记时，用 createdAt 时间判据 ---
        let no_marker = dir.join("session.v5.jsonl.zstd");
        {
            let f = std::fs::File::create(&no_marker).unwrap();
            let mut enc = zstd::stream::write::Encoder::new(f, 0).unwrap();
            let header = serde_json::json!({
                "type": "session",
                "version": 5,
                "id": "session-v5",
                "createdAt": created_at,
                "isSeeded": true
            });
            writeln!(enc, "{}", header).unwrap();
            writeln!(enc, "{}", msg(created_at - 60000, 1000.0)).unwrap();
            writeln!(enc, "{}", end_seed(false)).unwrap();
            writeln!(enc, "{}", msg(created_at + 5000, 8192.0)).unwrap();
            enc.finish().unwrap();
        }
        let records = parse_dsh_file(&no_marker).unwrap();
        assert_eq!(records.len(), 1, "无 inherited 标记时按 createdAt 丢弃继承内容");
        assert_eq!(records[0].total, 8192.0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
