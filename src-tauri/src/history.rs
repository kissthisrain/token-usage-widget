// 历史数据存储：%APPDATA%/token-usage-widget/history.json
// 结构与 core/history.js 完全对齐：
// days: { 'YYYY-MM-DD': { [sourceId]: { [model]: {tokens,input,output,cost,calls} } } }
// recent: [{ ts, sourceId, model, input, output, total, cost }]  上限 20000 条
// quota: { [sourceId]: { items, mode, error, at, ... } }
// fileOffsets: { [sourceId]: { [filePath]: bytes } }
// 写盘防抖 800ms + 原子写（tmp+rename）

use crate::config::atomic_write;
use crate::util::{date_key, days_ago_key, now_ms, today_key};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const RECENT_MAX: usize = 20000;
const RECENT_KEEP_DAYS: i64 = 35;
const SAVE_DEBOUNCE_MS: u64 = 800;

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct DayModel {
    #[serde(default)]
    pub tokens: f64,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    /// 美元费用（不含积分）
    #[serde(default)]
    pub cost: f64,
    /// 积分消耗（非货币，如 WorkBuddy）
    #[serde(default)]
    pub credits: f64,
    #[serde(default)]
    pub calls: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RecentRecord {
    pub ts: i64,
    pub source_id: String,
    pub model: String,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub total: f64,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub credits: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreditsInfo {
    #[serde(default)]
    pub has_credits: bool,
    #[serde(default)]
    pub unlimited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuotaItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<CreditsInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuotaInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default)]
    pub items: Vec<QuotaItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default)]
    pub at: i64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct History {
    #[serde(default = "default_version")]
    pub version: u64,
    #[serde(default)]
    pub days: BTreeMap<String, BTreeMap<String, BTreeMap<String, DayModel>>>,
    #[serde(default)]
    pub recent: Vec<RecentRecord>,
    #[serde(default)]
    pub quota: BTreeMap<String, QuotaInfo>,
    #[serde(default)]
    pub file_offsets: BTreeMap<String, BTreeMap<String, u64>>,
    /// dsh 已消费事件数：[sourceId -> [filePath -> assistant/message 事件数]]。
    /// dsh 为 zstd 压缩 JSONL，无法按字节偏移增量读，全量重读后用该计数取尾部增量
    ///（文件只追加、新事件总在尾部），重启不重复统计；截断重建时整源重置
    #[serde(default)]
    pub dsh_seen: BTreeMap<String, BTreeMap<String, u64>>,
    /// 「已累加进 days」的水位（按源 + 天）：`recover_seen[sourceId][day] = 最大 ts`。
    /// 由三处共同维护，确保同一批请求只被计入 days 一次：
    /// - sqlite 权威回填：[`HistoryStore::set_source_day_authoritative`] 把水位钉到该天 day_max；
    /// - 增量补齐：[`HistoryStore::recover_source_day_delta`] 只回收 `r.ts > 水位` 的 recent；
    /// - rollout 增量：[`HistoryStore::add_records`]（watermark=true）只接受 `ts > 水位`。
    /// 修复「启动时当天用量虚高、刷新后恢复」：旧实现在整体替换时把水位清空，
    /// rollout 全量重读（offset=0）便失去去重依据，把已落库的整批请求再累加一遍。
    /// 重建/清源时整源清零。
    #[serde(default)]
    pub recover_seen: BTreeMap<String, BTreeMap<String, i64>>,
    /// 一次性迁移水位：dsh seeded 会话（子代理 fork）过滤上线前，其文件开头继承的父会话历史
    /// 被逐条计入 days，导致该源历史虚高（实测 2026-10-08 多计 7.5M）。0 = 尚未迁移，
    /// 启动时对所有 dsh 源清空重扫一次；1 = 已完成。老 history.json 反序列化默认 0 → 自动迁移。
    #[serde(default)]
    pub dsh_seed_fix: u64,
    /// 一次性迁移水位：WorkBuddy 的 `rawUsage.credit` 是「积分」而非美元，旧实现误写入
    /// `cost`，导致积分被当成金额显示（实测 GPT-6-Luna 的 10.98 积分显示为 $10.98）。
    /// 0 = 尚未迁移，启动时对所有 workbuddy 源清空重扫一次（新解析器写入 credits）；
    /// 1 = 已完成。老 history.json 反序列化默认 0 → 自动迁移。
    #[serde(default)]
    pub workbuddy_credits_fix: u64,
    #[serde(default)]
    pub last_updated: i64,
}

fn default_version() -> u64 {
    1
}

/// 损坏文件备份：解析失败时保留原文件，防止后续 save 覆盖导致数据永久丢失
pub(crate) fn backup_corrupt(path: &Path) {
    if path.exists() {
        let bak = path.with_extension("json.corrupt");
        if let Err(e) = std::fs::copy(path, &bak) {
            log::warn!("[history] 备份损坏文件失败: {e}");
        }
    }
}

/// 归一化记录（addRecords 入参）
#[derive(Clone, Debug)]
pub struct RecordIn {
    pub ts: Option<i64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub total: f64,
    pub cache_read: Option<f64>,
    /// 费用（美元）。仅在线源与 API 返回的计价使用；与 `credits` 互斥。
    pub cost: Option<f64>,
    /// 积分消耗（非货币，如 WorkBuddy 的 `rawUsage.credit`）。
    /// 与 `cost` 分开累计，绝不混入美元合计。
    pub credits: Option<f64>,
}

pub struct HistoryStore {
    inner: Arc<Mutex<History>>,
    file: PathBuf,
    gen: Arc<AtomicU64>,
    /// 内存模式（冒烟测试用）：不读盘不写盘，避免污染真实数据
    no_persist: bool,
}

impl HistoryStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(History::default())),
            file: dir.join("history.json"),
            gen: Arc::new(AtomicU64::new(0)),
            no_persist: false,
        }
    }

    /// 冒烟测试用：纯内存，绝不落盘
    pub fn new_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(History::default())),
            file: PathBuf::from(""),
            gen: Arc::new(AtomicU64::new(0)),
            no_persist: true,
        }
    }

    pub fn load(&self) {
        if self.no_persist {
            return;
        }
        let h: History = match std::fs::read_to_string(&self.file) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(h) => h,
                Err(e) => {
                    // 损坏时备份原文件，避免后续 save 静默覆盖丢数据
                    log::warn!("[history] history.json 解析失败（{e}），已备份损坏文件");
                    backup_corrupt(&self.file);
                    History::default()
                }
            },
            Err(_) => History::default(),
        };
        *self.inner.lock().unwrap() = h;
    }

    /// 防抖写盘：800ms 内合并多次修改（对齐 JS scheduleSave）
    pub fn save_debounced(&self) {
        if self.no_persist {
            return;
        }
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let inner = self.inner.clone();
        let file = self.file.clone();
        let gen_ref = self.gen.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(SAVE_DEBOUNCE_MS)).await;
            if gen_ref.load(Ordering::SeqCst) != gen {
                return; // 已有更新的保存请求，由后者负责
            }
            let h = inner.lock().unwrap();
            if let Ok(json) = serde_json::to_string(&*h) {
                let _ = atomic_write(&file, &json);
            }
        });
    }

    /// 立即写盘（退出 / 清除等关键路径）
    pub fn save_now(&self) {
        if self.no_persist {
            return;
        }
        let h = self.inner.lock().unwrap();
        if let Ok(json) = serde_json::to_string(&*h) {
            let _ = atomic_write(&self.file, &json);
        }
    }

    pub fn data(&self) -> History {
        self.inner.lock().unwrap().clone()
    }

    pub fn touch(&self) {
        self.inner.lock().unwrap().last_updated = now_ms();
        self.save_debounced();
    }

    /// 记录归一化并累加；opts.use_recent=false 时只累加 days 不写 recent（API 全量源）
    ///
    /// `watermark=true`（zcode 等「以 sqlite 为权威」的源）：按 `recover_seen` 记录的
    /// 「该源-天已累加进 days 的最大 ts」去重，只接受 `ts > 水位` 的记录，并在**整批处理完**
    /// 之后把水位一次性推进到本批最大 ts。
    ///
    /// 水位按「批」推进而非逐条推进：毫秒级 ts 下同一批出现多条同 ts 请求很常见，逐条抬水位
    /// 会让第 2 条起被 `ts <= 水位` 跳过而漏计；改为整批处理完再推进，同批同 ts 记录都能计入。
    ///
    /// 这样 rollout 增量累加与 sqlite 整体替换会收敛到同一口径：即使启动时对当天 rollout
    /// 文件做全量重读（offset=0）、或收到重复的 notify 事件，已经落库的请求也不会被再叠加
    /// 一次 —— 修复「刚打开界面当天用量虚高、保存设置刷新后才恢复正常」的根因。
    pub fn add_records(
        &self,
        source_id: &str,
        records: Vec<RecordIn>,
        use_recent: bool,
        watermark: bool,
    ) {
        let mut changed = false;
        {
            let mut h = self.inner.lock().unwrap();
            let now = now_ms();
            // 批量起始水位快照：整批共用同一基准，避免「边处理边抬水位」把同一批内 ts 相同的
            // 记录误判为重复而漏计（毫秒级 ts 下同批多请求很常见）。水位在整批处理完后再推进。
            let start_ws: BTreeMap<String, i64> = if watermark {
                h.recover_seen.get(source_id).cloned().unwrap_or_default()
            } else {
                BTreeMap::new()
            };
            // 本批按天接受记录的最大 ts：整批处理结束后一次性并入 recover_seen
            let mut batch_max: BTreeMap<String, i64> = BTreeMap::new();
            for r in &records {
                if !r.total.is_finite() || r.total <= 0.0 {
                    continue;
                }
                let ts = r.ts.unwrap_or(now);
                let key = date_key(ts);
                if watermark {
                    // 已被 sqlite 回填（或前批累加）覆盖到该 ts → 跳过，避免 days/recent 重复计入
                    let prev = start_ws.get(&key).copied().unwrap_or(0);
                    if ts <= prev {
                        continue;
                    }
                    let m = batch_max.entry(key.clone()).or_insert(0);
                    if ts > *m {
                        *m = ts;
                    }
                }
                let model = r.model.as_deref().unwrap_or("(unknown)").to_string();
                let a = h
                    .days
                    .entry(key.clone())
                    .or_default()
                    .entry(source_id.to_string())
                    .or_default()
                    .entry(model.clone())
                    .or_default();
                a.tokens += r.total;
                a.input += r.input.unwrap_or(0.0);
                a.output += r.output.unwrap_or(0.0);
                if let Some(c) = r.cost {
                    if c.is_finite() {
                        a.cost += c;
                    }
                }
                if let Some(c) = r.credits {
                    if c.is_finite() {
                        a.credits += c;
                    }
                }
                a.calls += 1;
                if use_recent && ts >= now - RECENT_KEEP_DAYS * 86400000 {
                    h.recent.push(RecentRecord {
                        ts,
                        source_id: source_id.to_string(),
                        model: model.clone(),
                        input: r.input.unwrap_or(0.0),
                        output: r.output.unwrap_or(0.0),
                        total: r.total,
                        cost: r.cost.filter(|c| c.is_finite()),
                        credits: r.credits.filter(|c| c.is_finite()),
                    });
                }
                changed = true;
            }
            if watermark {
                // 整批处理完再推进水位：同批内 ts 相同的记录都已计入，不会互相误判为重复
                for (day, mx) in &batch_max {
                    let seen = h.recover_seen.entry(source_id.to_string()).or_default();
                    let cur = seen.entry(day.clone()).or_insert(0);
                    if *mx > *cur {
                        *cur = *mx;
                    }
                }
            }
            if changed {
                let cutoff = now - RECENT_KEEP_DAYS * 86400000;
                h.recent.retain(|r| r.ts >= cutoff);
                if h.recent.len() > RECENT_MAX {
                    let start = h.recent.len() - RECENT_MAX;
                    h.recent = h.recent[start..].to_vec();
                }
                h.last_updated = now;
            }
        }
        if changed {
            self.save_debounced();
        }
    }

    /// API 全量源：整体替换某天某源（幂等，防重复累计）；models 为空则删除该天该源
    pub fn set_source_day(
        &self,
        source_id: &str,
        day_key: &str,
        models: BTreeMap<String, DayModel>,
    ) {
        {
            let mut h = self.inner.lock().unwrap();
            let m = h.days.entry(day_key.to_string()).or_default();
            if models.is_empty() {
                m.remove(source_id);
            } else {
                m.insert(source_id.to_string(), models);
            }
            // 整体替换该天后，对应 recover 水位失效（旧的 seen 是基于已被覆盖的 days
            // 累计出来的）：清掉，避免后续 recover 漏掉新一批 recent 增量
            if let Some(seen_map) = h.recover_seen.get_mut(source_id) {
                seen_map.remove(day_key);
            }
            h.last_updated = now_ms();
        }
        self.save_debounced();
    }

    /// sqlite 权威回填某天：整体替换该天数据，并把「已累加水位」设为该天权威数据覆盖到的
    /// 最大 ts（`coverage_ts` = 该天 sqlite 的最大 completed_at）。
    ///
    /// 与 [`set_source_day`] 的关键差异：**不删除水位，而是把它钉在权威覆盖上界**。
    /// set_source_day 只做整体替换、把水位清空，于是后续 rollout 增量（add_records）
    /// 没有任何「已计过」的记忆：一旦对当天文件全量重读（offset=0），会把 sqlite 已经
    /// 计过的整批请求再累加一遍 → 当天用量虚高，直到下一次回填才自愈（实测 46M → 79M）。
    /// 这里把水位设为 day_max 后：
    /// - `add_records(watermark=true)` 跳过 `ts <= day_max` 的重复记录；
    /// - [`recover_source_day_delta`] 仍按 `ts > day_max` 从 recent 补回 sqlite 尚未落库的增量。
    /// 两者口径一致，与回填/扫描的先后顺序无关。
    pub fn set_source_day_authoritative(
        &self,
        source_id: &str,
        day_key: &str,
        models: BTreeMap<String, DayModel>,
        coverage_ts: i64,
    ) {
        {
            let mut h = self.inner.lock().unwrap();
            let m = h.days.entry(day_key.to_string()).or_default();
            if models.is_empty() {
                m.remove(source_id);
            } else {
                m.insert(source_id.to_string(), models);
            }
            if coverage_ts > 0 {
                h.recover_seen
                    .entry(source_id.to_string())
                    .or_default()
                    .insert(day_key.to_string(), coverage_ts);
            }
            h.last_updated = now_ms();
        }
        self.save_debounced();
    }

    /// 合并某天某源：按模型取各计数器的最大值（不覆盖已有值）。
    /// 用于 sqlite 权威回填与实时增量（rollout）混写同一天——两者都覆盖同一批请求，
    /// sqlite 通常更完整、rollout 能捕获最新未落库的记录，取 max 双向都安全（幂等，不丢数据）。
    /// 注意：max 只增不减，rollout 侧累计出虚高值后 sqlite 回填无法收敛；
    /// 新代码改用 set_source_day + recover_source_day_delta（sqlite 为准）。
    pub fn merge_source_day(
        &self,
        source_id: &str,
        day_key: &str,
        models: BTreeMap<String, DayModel>,
    ) {
        {
            let mut h = self.inner.lock().unwrap();
            let m = h.days.entry(day_key.to_string()).or_default();
            let src_map = m.entry(source_id.to_string()).or_default();
            for (model, dm) in models {
                let e = src_map.entry(model).or_default();
                e.tokens = e.tokens.max(dm.tokens);
                e.input = e.input.max(dm.input);
                e.output = e.output.max(dm.output);
                e.cost = e.cost.max(dm.cost);
                e.calls = e.calls.max(dm.calls);
            }
            h.last_updated = now_ms();
        }
        self.save_debounced();
    }

    /// sqlite 权威回填后的增量补齐：set_source_day_authoritative 按天整体替换会抹掉
    /// sqlite 尚未落库的最新记录（rollout 偏移已消费、无法重加），因此从 recent 找回
    /// 「超过该天 sqlite 最大 completed_at」的记录重新累加回 days（只写 days，不重复写入 recent）。
    /// day_cutoffs：天 → 该天 sqlite 最大 completed_at(ms)；只处理出现在其中的天。
    /// 口径：recent 与 sqlite 侧同口径（含缓存 raw 累计），只回收超出权威时点的新记录。
    ///
    /// 幂等性：每次回填调用都会跑一次该函数（启动期 spawn + h3 每 5 分钟一次），
    /// 而 recent 不被本函数修改，单纯按 day_max 筛选 candidates 会让同一批
    /// "超出 day_max 的 recent" 在多次调用中被重复累加 → days 持续虚高。
    /// 改用 `recover_seen[source_id][day]` 记录「该天已累计到 days 的最大 ts」（与
    /// add_records(watermark=true) 共用同一水位），candidates 只接受
    /// `r.ts > max(day_max, recover_seen)`，处理完后把当天水位推到本轮处理到的最大 ts。
    /// 回填整体替换某天时由 set_source_day_authoritative 把水位钉到 day_max；重建/清源整源清零。
    pub fn recover_source_day_delta(&self, source_id: &str, day_cutoffs: &BTreeMap<String, i64>) {
        if day_cutoffs.is_empty() {
            return;
        }
        let mut h = self.inner.lock().unwrap();
        // 复制"已累计水位"快照（避免与后续 recent 不可变借用冲突），
        // candidates 筛选用快照值；处理完成后再把新水位写回 recover_seen
        let prev_seen: BTreeMap<String, i64> = h
            .recover_seen
            .get(source_id)
            .map(|m| m.clone())
            .unwrap_or_default();
        let candidates: Vec<RecentRecord> = h
            .recent
            .iter()
            .filter(|r| {
                if r.source_id != source_id {
                    return false;
                }
                let day = date_key(r.ts);
                let Some(max_ts) = day_cutoffs.get(&day).copied() else {
                    return false;
                };
                let prev = prev_seen.get(&day).copied().unwrap_or(0);
                r.ts > max_ts && r.ts > prev
            })
            .cloned()
            .collect();
        if candidates.is_empty() {
            return;
        }
        let now = now_ms();
        // 记录本轮按天实际处理到的最大 ts，回写给 seen_map
        let mut new_seen: BTreeMap<String, i64> = BTreeMap::new();
        for r in &candidates {
            let day = date_key(r.ts);
            let cur = new_seen.entry(day).or_insert(0);
            if r.ts > *cur {
                *cur = r.ts;
            }
        }
        for r in &candidates {
            let day = date_key(r.ts);
            let model = &r.model;
            let a = h
                .days
                .entry(day.clone())
                .or_default()
                .entry(source_id.to_string())
                .or_default()
                .entry(model.clone())
                .or_default();
            a.tokens += r.total;
            a.input += r.input;
            a.output += r.output;
            if let Some(c) = r.cost {
                if c.is_finite() {
                    a.cost += c;
                }
            }
            if let Some(c) = r.credits {
                if c.is_finite() {
                    a.credits += c;
                }
            }
            a.calls += 1;
        }
        let seen_map = h.recover_seen.entry(source_id.to_string()).or_default();
        for (day, ts) in new_seen {
            let cur = seen_map.entry(day).or_insert(0);
            if ts > *cur {
                *cur = ts;
            }
        }
        h.last_updated = now;
        drop(h);
        self.save_debounced();
    }

    /// 清除某源全部数据（days 的所有天 + recent 中该源记录）。
    /// 供自愈/截断重建使用：重建前必须清干净，否则重扫会把旧记录再累加一遍
    pub fn clear_source_all(&self, source_id: &str) {
        let mut h = self.inner.lock().unwrap();
        let mut changed = false;
        let keys: Vec<String> = h.days.keys().cloned().collect();
        for k in keys {
            if let Some(day) = h.days.get_mut(&k) {
                if day.remove(source_id).is_some() {
                    changed = true;
                }
            }
        }
        let before = h.recent.len();
        h.recent.retain(|r| r.source_id != source_id);
        if h.recent.len() != before {
            changed = true;
        }
        // 整源清数据后，recover 水位也失效（days 与 recent 都已清空，旧水位不再适用）
        if h.recover_seen.remove(source_id).is_some() {
            changed = true;
        }
        if changed {
            h.last_updated = now_ms();
        }
    }

    /// 清理已从配置中删除的源：days / recent / quota / file_offsets / dsh_seen / recover_seen
    /// 中所有 sourceId 不在 `valid_ids` 的记录。
    ///
    /// 用户反复「删除源 → 重新添加」时每次都会生成新的 sourceId，旧 id 的数据会永久残留在
    /// history.json 里（每个 id 还带着一份 dshSeen / fileOffsets）；实测 residue 三个 dsh id
    /// 各占 ~100M 记录。这些残留不会污染界面（build_snapshot 只按 config 里的 id 聚合），
    /// 但会持续放大 history.json。启动时调用一次即可。
    ///
    /// `valid_ids` 为空时直接返回 false：配置里一个源都没有（首次运行 / 用户手动清空配置）时
    /// 不能把历史全判为残留删掉，否则等于静默清库。
    ///
    /// 返回是否发生变化（调用方据此决定是否落盘）。
    pub fn purge_stale_sources(&self, valid_ids: &HashSet<String>) -> bool {
        if valid_ids.is_empty() {
            return false;
        }
        let mut h = self.inner.lock().unwrap();
        let mut changed = false;

        for day in h.days.values_mut() {
            let stale: Vec<String> = day
                .keys()
                .filter(|k| !valid_ids.contains(k.as_str()))
                .cloned()
                .collect();
            for k in stale {
                day.remove(&k);
                changed = true;
            }
        }
        // 天被清空后删掉空壳天，避免 history.json 里留下 "{}" 占位
        let empty_days: Vec<String> = h
            .days
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        for k in empty_days {
            h.days.remove(&k);
            changed = true;
        }

        let before = h.recent.len();
        h.recent.retain(|r| valid_ids.contains(r.source_id.as_str()));
        if h.recent.len() != before {
            changed = true;
        }

        // 各 id 索引表的 value 类型不同（QuotaInfo / BTreeMap<String,u64> / BTreeMap<String,i64>），
        // 用一个只关心 key 的泛型小工具逐个清理
        fn drop_unknown<V>(map: &mut BTreeMap<String, V>, valid_ids: &HashSet<String>) -> bool {
            let stale: Vec<String> = map
                .keys()
                .filter(|k| !valid_ids.contains(k.as_str()))
                .cloned()
                .collect();
            let hit = !stale.is_empty();
            for k in stale {
                map.remove(&k);
            }
            hit
        }
        changed |= drop_unknown(&mut h.quota, valid_ids);
        changed |= drop_unknown(&mut h.file_offsets, valid_ids);
        changed |= drop_unknown(&mut h.dsh_seen, valid_ids);
        changed |= drop_unknown(&mut h.recover_seen, valid_ids);

        if changed {
            h.last_updated = now_ms();
        }
        changed
    }

    pub fn set_quota(&self, source_id: &str, info: QuotaInfo) {
        {
            let mut h = self.inner.lock().unwrap();
            let mut q = info;
            q.at = now_ms();
            h.quota.insert(source_id.to_string(), q);
            h.last_updated = now_ms();
        }
        self.save_debounced();
    }

    pub fn clear(&self) {
        {
            let mut h = self.inner.lock().unwrap();
            h.days.clear();
            h.recent.clear();
            h.quota.clear();
            h.file_offsets.clear();
            h.dsh_seen.clear();
            h.last_updated = 0;
        }
        self.save_now();
    }

    pub fn get_file_offset(&self, source_id: &str, file: &str) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .file_offsets
            .get(source_id)
            .and_then(|m| m.get(file))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_file_offset(&self, source_id: &str, file: &str, bytes: u64) {
        self.inner
            .lock()
            .unwrap()
            .file_offsets
            .entry(source_id.to_string())
            .or_default()
            .insert(file.to_string(), bytes);
    }

    /// dsh：某文件已消费的 assistant/message 事件数（默认 0）
    pub fn get_dsh_seen(&self, source_id: &str, file: &str) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .dsh_seen
            .get(source_id)
            .and_then(|m| m.get(file))
            .copied()
            .unwrap_or(0)
    }

    /// dsh：记录某文件已消费的事件总数（全量重读后调用；与 file_offsets 同随防抖写盘持久化）
    pub fn set_dsh_seen(&self, source_id: &str, file: &str, count: u64) {
        self.inner
            .lock()
            .unwrap()
            .dsh_seen
            .entry(source_id.to_string())
            .or_default()
            .insert(file.to_string(), count);
    }

    /// dsh：截断/重建时整源重置计数（下次全量重读重新计数）
    pub fn reset_dsh_seen(&self, source_id: &str) {
        let mut h = self.inner.lock().unwrap();
        if h.dsh_seen.remove(source_id).is_some() {
            h.last_updated = now_ms();
        }
    }

    /// dsh seeded 过滤迁移是否已完成（见 History::dsh_seed_fix）
    pub fn get_dsh_seed_fix(&self) -> u64 {
        self.inner.lock().unwrap().dsh_seed_fix
    }

    /// 标记 dsh seeded 过滤迁移已完成并立即落盘（避免迁移后被重复执行）
    pub fn set_dsh_seed_fix(&self) {
        {
            let mut h = self.inner.lock().unwrap();
            h.dsh_seed_fix = 1;
        }
        self.save_now();
    }

    /// WorkBuddy 积分口径迁移是否已完成（见 History::workbuddy_credits_fix）
    pub fn get_workbuddy_credits_fix(&self) -> u64 {
        self.inner.lock().unwrap().workbuddy_credits_fix
    }

    /// 标记 WorkBuddy 积分口径迁移已完成并立即落盘（避免迁移后被重复执行）
    pub fn set_workbuddy_credits_fix(&self) {
        {
            let mut h = self.inner.lock().unwrap();
            h.workbuddy_credits_fix = 1;
        }
        self.save_now();
    }

    // ---------- 聚合 ----------

    #[allow(clippy::type_complexity)]
    pub fn aggregate(
        &self,
        source_ids: &[String],
        range: &str,
        whitelist: &HashMap<String, HashSet<String>>,
    ) -> AggregateResult {
        let h = self.inner.lock().unwrap();
        let keys: Vec<String> = h.days.keys().cloned().collect();
        let now = now_ms();
        let today = today_key();
        let month_start = days_ago_key(29);
        let in_range = |k: &String| -> bool {
            if range == "day" {
                k >= &today
            } else if range == "month" {
                k >= &month_start
            } else {
                true
            }
        };
        let ids: HashSet<&String> = source_ids.iter().collect();
        let allow_model = |sid: &String, model: &str| -> bool {
            match whitelist.get(sid) {
                Some(wl) => wl.contains(model),
                None => true,
            }
        };

        let mut tokens: f64 = 0.0;
        let mut cost: f64 = 0.0;
        let mut credits: f64 = 0.0;
        let mut cost_partial = false;
        // `${sid}|${model}` → ModelAgg（聚合期无 sourceName）
        let mut model_map: HashMap<String, ModelAgg> = HashMap::new();
        let mut trend_map: BTreeMap<String, TrendPoint> = BTreeMap::new();
        let mut active_set: HashSet<String> = HashSet::new();

        for k in &keys {
            if !in_range(k) {
                continue;
            }
            let Some(day) = h.days.get(k) else { continue };
            let mut day_tokens: f64 = 0.0;
            for sid in day.keys() {
                if !ids.contains(sid) {
                    continue;
                }
                let src = &day[sid];
                for model in src.keys() {
                    if !allow_model(sid, model) {
                        continue;
                    }
                    let a = &src[model];
                    day_tokens += a.tokens;
                    if a.tokens > 0.0 {
                        active_set.insert((*k).clone());
                    }
                    let mk = format!("{sid}|{model}");
                    let mm = model_map.entry(mk).or_insert_with(|| ModelAgg {
                        model: model.clone(),
                        source_id: sid.clone(),
                        source_name: None,
                        tokens: 0.0,
                        cost: 0.0,
                        credits: 0.0,
                        cost_partial: false,
                        calls: 0,
                        pct: 0,
                    });
                    mm.tokens += a.tokens;
                    mm.cost += a.cost;
                    mm.credits += a.credits;
                    mm.calls += a.calls;
                }
            }
            if day_tokens > 0.0 {
                let t = trend_map.entry((*k).clone()).or_insert_with(|| TrendPoint {
                    date: (*k).clone(),
                    tokens: 0.0,
                    cost: 0.0,
                    cost_partial: false,
                });
                t.tokens += day_tokens;
            }
        }

        // 热力图：最近 26 周每天（不随 range 过滤，展示全量活动）
        let heat_start = days_ago_key(26 * 7 - 1);
        let mut heatmap_map: HashMap<String, f64> = HashMap::new();
        for k in &keys {
            if *k < heat_start {
                continue;
            }
            if let Some(day) = h.days.get(k) {
                let mut day_tokens: f64 = 0.0;
                for sid in day.keys() {
                    if !ids.contains(sid) {
                        continue;
                    }
                    for (model, a) in &day[sid] {
                        if allow_model(sid, model) {
                            day_tokens += a.tokens;
                        }
                    }
                }
                if day_tokens > 0.0 {
                    heatmap_map.insert((*k).clone(), day_tokens);
                }
            }
        }
        // 活跃天数：全量口径（与热力图一致，不受 range 过滤）
        let mut active_days_all: HashSet<String> = HashSet::new();
        for k in &keys {
            if let Some(day) = h.days.get(k) {
                for sid in day.keys() {
                    if !ids.contains(sid) {
                        continue;
                    }
                    let mut has = false;
                    for (model, a) in &day[sid] {
                        if allow_model(sid, model) && a.tokens > 0.0 {
                            has = true;
                            break;
                        }
                    }
                    if has {
                        active_days_all.insert((*k).clone());
                    }
                }
            }
        }
        let active_days = active_days_all.len();

        for mm in model_map.values() {
            tokens += mm.tokens;
            cost += mm.cost;
            credits += mm.credits;
        }
        // recent 中 cost 为 null 的记录 → costPartial（只统计当前筛选范围内，
        // 避免 35 天前的一笔无价记录让 DAY 视图也显示"~"）
        let range_cutoff: i64 = if range == "day" {
            crate::util::today_start_ms()
        } else if range == "month" {
            now - 29 * 86400000
        } else {
            0
        };
        for r in &h.recent {
            if r.cost.is_none() && ids.contains(&r.source_id) && r.ts >= range_cutoff {
                cost_partial = true;
            }
        }

        let mut models: Vec<ModelAgg> = model_map.into_values().collect();
        models.sort_by(|a, b| b.tokens.partial_cmp(&a.tokens).unwrap_or(std::cmp::Ordering::Equal));
        let max_tokens = models.first().map(|m| m.tokens).unwrap_or(0.0);
        for mm in models.iter_mut() {
            mm.pct = if max_tokens > 0.0 {
                ((mm.tokens / max_tokens) * 100.0).round() as i64
            } else {
                0
            };
        }

        let trend: Vec<TrendPoint> = trend_map.into_values().collect();

        // 热力图数据：最近 26 周每天（升序，无活动为 0）
        let mut heatmap: Vec<HeatmapPoint> = Vec::new();
        for i in 0..(26 * 7) {
            let k = date_key(now_ms() - (i as i64) * 86400000);
            if k < heat_start {
                break;
            }
            let tokens = heatmap_map.get(&k).copied().unwrap_or(0.0);
            heatmap.push(HeatmapPoint { date: k, tokens });
        }
        heatmap.reverse();

        AggregateResult {
            totals: Totals {
                tokens,
                cost,
                credits,
                cost_partial,
            },
            models,
            trend,
            heatmap,
            active_days,
        }
    }

    /// opencode-go 类：基于历史算窗口内美元消耗
    pub fn spent_in_window(&self, source_id: &str, ms: i64) -> (f64, bool, Option<i64>) {
        let h = self.inner.lock().unwrap();
        let cutoff = now_ms() - ms;
        let mut spent: f64 = 0.0;
        let mut partial = false;
        let mut first_ts: Option<i64> = None;
        for r in &h.recent {
            if r.source_id != source_id || r.ts < cutoff {
                continue;
            }
            match r.cost {
                None => {
                    partial = true;
                    continue;
                }
                Some(c) => {
                    spent += c;
                    if first_ts.is_none_or(|f| r.ts < f) {
                        first_ts = Some(r.ts);
                    }
                }
            }
        }
        // recent 保留 35 天，覆盖 30 天月窗口
        if ms > RECENT_KEEP_DAYS * 86400000 {
            partial = true;
        }
        (spent, partial, first_ts)
    }

    pub fn spent_in_calendar_month(&self, source_id: &str) -> (f64, bool) {
        let h = self.inner.lock().unwrap();
        let month_key = &today_key()[..7];
        let mut spent: f64 = 0.0;
        for (k, day) in &h.days {
            if !k.starts_with(month_key) {
                continue;
            }
            if let Some(src) = day.get(source_id) {
                for a in src.values() {
                    spent += a.cost;
                }
            }
        }
        // 月窗口是否含未计费记录（未知模型/无价目）——recent 保留 35 天覆盖整个自然月
        let mut partial = false;
        for r in &h.recent {
            if r.source_id == source_id && r.cost.is_none() && date_key(r.ts).starts_with(month_key) {
                partial = true;
                break;
            }
        }
        (spent, partial)
    }

    pub fn reset_file_offsets(&self, source_id: &str, files: &[String]) {
        let mut h = self.inner.lock().unwrap();
        let m = h.file_offsets.entry(source_id.to_string()).or_default();
        for f in files {
            m.insert(f.clone(), 0);
        }
    }

    /// 自愈清数据前快照备份：history.json.pre-heal.bak（仅文件存在时；误判时可手工恢复）
    pub fn backup_before_heal(&self) {
        if self.no_persist {
            return;
        }
        if self.file.exists() {
            let _ = std::fs::copy(&self.file, self.file.with_extension("json.pre-heal.bak"));
        }
    }

    /// dsh seeded 过滤迁移前快照备份：history.json.pre-seed-fix.bak
    ///（该迁移会清空 dsh 源历史并重扫，保留旧文件便于核对/回退）
    pub fn backup_before_seed_fix(&self) {
        if self.no_persist {
            return;
        }
        if self.file.exists() {
            let _ = std::fs::copy(&self.file, self.file.with_extension("json.pre-seed-fix.bak"));
        }
    }

    /// WorkBuddy 积分口径迁移前快照备份：history.json.pre-wb-credits.bak
    ///（该迁移会清空 workbuddy 源历史并重扫，保留旧文件便于核对/回退）
    pub fn backup_before_wb_credits_fix(&self) {
        if self.no_persist {
            return;
        }
        if self.file.exists() {
            let _ = std::fs::copy(&self.file, self.file.with_extension("json.pre-wb-credits.bak"));
        }
    }
}

// ---------- 聚合返回结构 ----------

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub tokens: f64,
    /// 美元费用合计（不含积分）
    pub cost: f64,
    /// 积分消耗合计（非货币）
    pub credits: f64,
    pub cost_partial: bool,
}

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelAgg {
    pub model: String,
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    pub tokens: f64,
    pub cost: f64,
    /// 积分消耗（非货币，如 WorkBuddy）；与 cost 分开，不参与美元合计
    pub credits: f64,
    pub cost_partial: bool,
    pub calls: u64,
    pub pct: i64,
}

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TrendPoint {
    pub date: String,
    pub tokens: f64,
    pub cost: f64,
    pub cost_partial: bool,
}

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HeatmapPoint {
    pub date: String,
    pub tokens: f64,
}

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AggregateResult {
    pub totals: Totals,
    pub models: Vec<ModelAgg>,
    pub trend: Vec<TrendPoint>,
    pub heatmap: Vec<HeatmapPoint>,
    pub active_days: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一条只带必要字段的 recent（其余字段默认）
    fn rec(ts: i64, source_id: &str, model: &str, total: f64) -> RecentRecord {
        RecentRecord {
            ts,
            source_id: source_id.to_string(),
            model: model.to_string(),
            input: 0.0,
            output: 0.0,
            total,
            cost: None,
            credits: None,
        }
    }

    fn day_total(history: &HistoryStore, day: &str, source_id: &str) -> f64 {
        let h = history.data();
        h.days
            .get(day)
            .and_then(|m| m.get(source_id))
            .map(|models| models.values().map(|a| a.tokens).sum())
            .unwrap_or(0.0)
    }

    /// 构造一条 RecordIn（测试用）
    fn ri(ts: i64, model: &str, total: f64) -> RecordIn {
        RecordIn {
            ts: Some(ts),
            model: Some(model.to_string()),
            provider: None,
            input: Some(0.0),
            output: Some(0.0),
            total,
            cache_read: None,
            cost: None,
            credits: None,
        }
    }

    fn day_model(tokens: f64, calls: u64) -> DayModel {
        DayModel {
            tokens,
            input: 0.0,
            output: 0.0,
            cost: 0.0,
            credits: 0.0,
            calls,
        }
    }

    /// 回归：积分（credits）与美元费用（cost）必须分开累计，绝不混入同一合计
    /// （WorkBuddy 的 rawUsage.credit 是积分，曾被误当美元显示为 $19.20）
    #[test]
    fn credits_stay_separate_from_cost() {
        let store = HistoryStore::new_memory();
        store.add_records(
            "workbuddy-intl",
            vec![RecordIn {
                ts: Some(crate::util::now_ms()),
                model: Some("m".into()),
                provider: None,
                input: Some(0.0),
                output: Some(0.0),
                total: 10.0,
                cache_read: None,
                cost: None,
                credits: Some(3.5),
            }],
            true,
            false,
        );
        let day = crate::util::date_key(crate::util::now_ms());
        let h = store.data();
        let a = &h.days[&day]["workbuddy-intl"]["m"];
        assert_eq!(a.tokens, 10.0);
        assert_eq!(a.cost, 0.0, "积分不得计入美元费用");
        assert_eq!(a.credits, 3.5, "积分应单独累计");
    }

    /// 关键回归：recover 多次调用不应重复累加同一批 recent
    #[test]
    fn recover_is_idempotent_across_calls() {
        let store = HistoryStore::new_memory();
        let sid = "zcode";
        // 模拟 today 有 3 条 recent（h2 已经写进 recent）
        store.add_records(
            sid,
            vec![
                RecordIn {
                    ts: Some(1_000),
                    model: Some("m".into()),
                    provider: None,
                    input: Some(0.0),
                    output: Some(0.0),
                    total: 100.0,
                    cache_read: None,
                    cost: None,
                    credits: None,
                },
                RecordIn {
                    ts: Some(2_000),
                    model: Some("m".into()),
                    provider: None,
                    input: Some(0.0),
                    output: Some(0.0),
                    total: 200.0,
                    cache_read: None,
                    cost: None,
                    credits: None,
                },
                RecordIn {
                    ts: Some(3_000),
                    model: Some("m".into()),
                    provider: None,
                    input: Some(0.0),
                    output: Some(0.0),
                    total: 300.0,
                    cache_read: None,
                    cost: None,
                    credits: None,
                },
            ],
            true,
            false,
        );
        // sqlite 端尚未 commit → day_max = 0（cutoffs 不能为空，否则函数早 return）
        let cutoffs: BTreeMap<String, i64> =
            vec![("1970-01-01".to_string(), 0i64)].into_iter().collect();

        // 第一次回填：candidates = 全部 3 条（ts 都 > max(0, 0)），days[today] += 600
        store.recover_source_day_delta(sid, &cutoffs);
        assert!((day_total(&store, "1970-01-01", sid) - 600.0).abs() < 1e-9);

        // 第二次回填：水位已经把今天处理到 ts=3000，candidates 应为空，days 不变
        store.recover_source_day_delta(sid, &cutoffs);
        assert!((day_total(&store, "1970-01-01", sid) - 600.0).abs() < 1e-9);

        // 第三次回填：仍然不变
        store.recover_source_day_delta(sid, &cutoffs);
        assert!((day_total(&store, "1970-01-01", sid) - 600.0).abs() < 1e-9);
    }

    /// 关键回归：sqlite 推进 day_max 后，只有更新过的新 recent 才会被 recover
    #[test]
    fn recover_skips_records_already_in_sqlite() {
        let store = HistoryStore::new_memory();
        let sid = "zcode";
        // recent：4 条（按 ts 升序）
        let mut manual = Vec::new();
        for (ts, total) in [(1000, 10.0), (2000, 20.0), (3000, 30.0), (4000, 40.0)] {
            manual.push(rec(ts, sid, "m", total));
        }
        {
            let mut h = store.inner.lock().unwrap();
            h.recent = manual;
        }

        // 第一轮回填：day_max = 0（sqlite 端还没任何记录）→ candidates = 全部 4 条
        let mut cutoffs = BTreeMap::new();
        cutoffs.insert("1970-01-01".to_string(), 0i64);
        store.recover_source_day_delta(sid, &cutoffs);
        assert!((day_total(&store, "1970-01-01", sid) - 100.0).abs() < 1e-9);

        // 模拟 sqlite 已经 commit 了前 2 条 → day_max[today] = 2000
        cutoffs.insert("1970-01-01".to_string(), 2_000);

        // set_source_day_authoritative 整体替换 days（含 SQLite 侧 30），并把水位钉到 day_max=2000
        // （生产路径 zcode 回填即走此函数；不再像旧 set_source_day 那样清空水位）
        let mut models = BTreeMap::new();
        models.insert(
            "m".to_string(),
            DayModel {
                tokens: 30.0,
                input: 0.0,
                output: 0.0,
                cost: 0.0,
                credits: 0.0,
                calls: 2,
            },
        );
        store.set_source_day_authoritative(sid, "1970-01-01", models, 2_000);

        // 第二轮回填：day_max=2000、水位=2000；
        // candidates = ts>2000 的两条（3000,4000）→ days += 30+40 = 70
        // 最终 days = 30(sqlite) + 70(recover) = 100，与第一轮后持平
        store.recover_source_day_delta(sid, &cutoffs);
        assert!(
            (day_total(&store, "1970-01-01", sid) - 100.0).abs() < 1e-9,
            "期望 days=100，实际 {}",
            day_total(&store, "1970-01-01", sid)
        );

        // 第三轮回填：水位已推到 ts=4000，candidates 为空 → days 不变
        store.recover_source_day_delta(sid, &cutoffs);
        assert!((day_total(&store, "1970-01-01", sid) - 100.0).abs() < 1e-9);
    }

    /// 根因回归：sqlite 权威回填后，rollout 全量重读（offset=0）不得把已落库的当天数据再叠加。
    /// 复现线上现象 —— 界面先显示 46M+33M≈79M（虚高），设置页刷新/周期回填后才恢复 46M。
    #[test]
    fn rollout_reread_skipped_after_authoritative_backfill() {
        let store = HistoryStore::new_memory();
        let sid = "zcode";
        let day = "1970-01-01";

        // 1) sqlite 权威回填：当天 3 笔、合计 60、day_max = 3000
        let mut models = BTreeMap::new();
        models.insert("m".to_string(), day_model(60.0, 3));
        store.set_source_day_authoritative(sid, day, models, 3_000);
        assert!((day_total(&store, day, sid) - 60.0).abs() < 1e-9);

        // 2) rollout 全量重读：同一批请求（ts 1000/2000/3000）再次送入 → 全部被水位拦下
        store.add_records(
            sid,
            vec![ri(1_000, "m", 10.0), ri(2_000, "m", 20.0), ri(3_000, "m", 30.0)],
            true,
            true,
        );
        assert!(
            (day_total(&store, day, sid) - 60.0).abs() < 1e-9,
            "重复重读不应叠加，实际 {}",
            day_total(&store, day, sid)
        );

        // 3) 真正的新记录（ts > 水位）仍要计入
        store.add_records(sid, vec![ri(4_000, "m", 40.0)], true, true);
        assert!((day_total(&store, day, sid) - 100.0).abs() < 1e-9);

        // 4) 再次全量重读（含上一步的新记录）→ 仍不叠加（水位已推高到 4000）
        store.add_records(
            sid,
            vec![
                ri(1_000, "m", 10.0),
                ri(2_000, "m", 20.0),
                ri(3_000, "m", 30.0),
                ri(4_000, "m", 40.0),
            ],
            true,
            true,
        );
        assert!((day_total(&store, day, sid) - 100.0).abs() < 1e-9);

        // 5) 非权威源（watermark=false）行为不变：重复送入仍按原语义累加
        let s2 = "codex";
        store.add_records(s2, vec![ri(1_000, "m", 10.0)], true, false);
        store.add_records(s2, vec![ri(1_000, "m", 10.0)], true, false);
        assert!((day_total(&store, day, s2) - 20.0).abs() < 1e-9);
    }

    /// 关键回归：sqlite 后续回填抬高 day_max 时，水位钉到新 day_max；仍不重复计入
    #[test]
    fn authoritative_backfill_keeps_watermark_monotonic() {
        let store = HistoryStore::new_memory();
        let sid = "zcode";
        let day = "1970-01-01";
        // 首轮：sqlite 只有 1 笔（day_max=1000）
        let mut m1 = BTreeMap::new();
        m1.insert("m".to_string(), day_model(10.0, 1));
        store.set_source_day_authoritative(sid, day, m1, 1_000);
        // rollout 送 2 笔（一对已落库、一笔新）
        store.add_records(sid, vec![ri(1_000, "m", 10.0), ri(2_000, "m", 20.0)], true, true);
        assert!((day_total(&store, day, sid) - 30.0).abs() < 1e-9);

        // 第二轮：sqlite 追平（2 笔，day_max=2000）→ 整体替换 + 水位推到 2000
        let mut m2 = BTreeMap::new();
        m2.insert("m".to_string(), day_model(30.0, 2));
        store.set_source_day_authoritative(sid, day, m2, 2_000);
        // recover 从 recent 补回 ts>2000 的记录（此处无）→ 保持 30
        let mut cutoffs = BTreeMap::new();
        cutoffs.insert(day.to_string(), 2_000i64);
        store.recover_source_day_delta(sid, &cutoffs);
        assert!((day_total(&store, day, sid) - 30.0).abs() < 1e-9);
        // 全量重读仍不叠加
        store.add_records(sid, vec![ri(1_000, "m", 10.0), ri(2_000, "m", 20.0)], true, true);
        assert!((day_total(&store, day, sid) - 30.0).abs() < 1e-9);
    }

    /// 回归：同一批内 ts 相同的多条记录不得互相去重（水位按批推进，而非逐条）
    #[test]
    fn same_ts_records_in_one_batch_are_all_counted() {
        let store = HistoryStore::new_memory();
        let sid = "zcode";
        let day = "1970-01-01";
        // 同一毫秒两条请求：都计入（旧实现逐条抬水位会把第 2 条跳过 → 只算 10）
        store.add_records(
            sid,
            vec![ri(1_000, "m", 10.0), ri(1_000, "m", 20.0)],
            true,
            true,
        );
        assert!(
            (day_total(&store, day, sid) - 30.0).abs() < 1e-9,
            "同批同 ts 应全部计入，实际 {}",
            day_total(&store, day, sid)
        );
        // 整批全量重读仍不叠加（水位已推进到 1000）
        store.add_records(
            sid,
            vec![ri(1_000, "m", 10.0), ri(1_000, "m", 20.0)],
            true,
            true,
        );
        assert!((day_total(&store, day, sid) - 30.0).abs() < 1e-9);
        // 新的一条（ts 更大）继续计入
        store.add_records(sid, vec![ri(2_000, "m", 5.0)], true, true);
        assert!((day_total(&store, day, sid) - 35.0).abs() < 1e-9);
    }

    /// clear_source_all 应当同时清掉 recover_seen 整源水位
    #[test]
    fn clear_source_drops_recover_seen() {
        let store = HistoryStore::new_memory();
        let sid = "zcode";
        // 先手动放一条 recent，触发一次 recover，让 seen_map 写入
        {
            let mut h = store.inner.lock().unwrap();
            h.recent.push(rec(1000, sid, "m", 50.0));
        }
        // cutoffs 必须非空（函数会在空时早 return），用 day_max=0 让 candidates 入选
        let mut cutoffs = BTreeMap::new();
        cutoffs.insert("1970-01-01".to_string(), 0i64);
        store.recover_source_day_delta(sid, &cutoffs);
        assert!(
            store
                .data()
                .recover_seen
                .get(sid)
                .map(|m| !m.is_empty())
                .unwrap_or(false),
            "recover 后 seen_map 应非空"
        );
        store.clear_source_all(sid);
        assert!(
            store.data().recover_seen.get(sid).is_none(),
            "clear_source_all 后 seen_map 应整源消失"
        );
    }

    /// purge_stale_sources：清掉配置里已不存在的源，保留有效源；空 valid 集合时不误删
    #[test]
    fn purge_stale_sources_drops_only_unknown_ids() {
        let store = HistoryStore::new_memory();
        // 有效源 keep + 残留源 stale，两者都写进 days / recent / dsh_seen / file_offsets / quota
        store.add_records(
            "keep",
            vec![RecordIn {
                ts: Some(1_000),
                model: Some("m".into()),
                provider: None,
                input: None,
                output: None,
                total: 100.0,
                cache_read: None,
                cost: None,
                credits: None,
            }],
            true,
            false,
        );
        store.add_records(
            "stale",
            vec![RecordIn {
                ts: Some(1_000),
                model: Some("m".into()),
                provider: None,
                input: None,
                output: None,
                total: 999.0,
                cache_read: None,
                cost: None,
                credits: None,
            }],
            true,
            false,
        );
        store.set_dsh_seen("stale", "f.jsonl.zstd", 7);
        store.set_dsh_seen("keep", "f.jsonl.zstd", 3);
        store.set_file_offset("stale", "f.jsonl.zstd", 42);
        store.set_file_offset("keep", "f.jsonl.zstd", 42);
        store.set_quota(
            "stale",
            QuotaInfo {
                items: vec![],
                ..Default::default()
            },
        );

        // 空集合保护：一个源都没配置时不得清库
        assert!(!store.purge_stale_sources(&HashSet::new()));
        assert!((day_total(&store, "1970-01-01", "stale") - 999.0).abs() < 1e-9);

        let valid: HashSet<String> = ["keep".to_string()].into_iter().collect();
        assert!(store.purge_stale_sources(&valid));

        let h = store.data();
        assert!(
            (day_total(&store, "1970-01-01", "keep") - 100.0).abs() < 1e-9,
            "有效源数据必须保留"
        );
        for day in h.days.values() {
            assert!(day.get("stale").is_none(), "残留源应从 days 消失");
        }
        assert!(h.recent.iter().all(|r| r.source_id == "keep"));
        assert!(h.dsh_seen.get("stale").is_none());
        assert!(h.file_offsets.get("stale").is_none());
        assert!(h.quota.get("stale").is_none());
        // 有效源的计数不能被顺手清掉
        assert_eq!(h.dsh_seen["keep"]["f.jsonl.zstd"], 3);
        assert_eq!(h.file_offsets["keep"]["f.jsonl.zstd"], 42);

        // 幂等：再跑一次不应报告变化
        assert!(!store.purge_stale_sources(&valid));
    }
}
