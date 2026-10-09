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
    #[serde(default)]
    pub cost: f64,
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
    /// sqlite 权威回填后已 recover 过的最大 ts（按源 + 天）。
    /// recover 只处理 `r.ts > max(sqlite day_max, 本字段记录值)` 的 recent 记录，
    /// 防止同一批 recent 在多次回填中被重复累加到 days（5 分钟一次的 h3 周期回填 + 启动
    /// 时立即 spawn 一次 → 不去重会让 days 持续虚高）。整体替换某天时（set_source_day）
    /// 同源该天水位清零；重建/清源时整源清零。
    #[serde(default)]
    pub recover_seen: BTreeMap<String, BTreeMap<String, i64>>,
    /// 一次性迁移水位：dsh seeded 会话（子代理 fork）过滤上线前，其文件开头继承的父会话历史
    /// 被逐条计入 days，导致该源历史虚高（实测 2026-10-08 多计 7.5M）。0 = 尚未迁移，
    /// 启动时对所有 dsh 源清空重扫一次；1 = 已完成。老 history.json 反序列化默认 0 → 自动迁移。
    #[serde(default)]
    pub dsh_seed_fix: u64,
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
    pub cost: Option<f64>,
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
    pub fn add_records(&self, source_id: &str, records: Vec<RecordIn>, use_recent: bool) {
        let mut changed = false;
        {
            let mut h = self.inner.lock().unwrap();
            let now = now_ms();
            for r in &records {
                if !r.total.is_finite() || r.total <= 0.0 {
                    continue;
                }
                let ts = r.ts.unwrap_or(now);
                let key = date_key(ts);
                let model = r.model.as_deref().unwrap_or("(unknown)").to_string();
                let a = h
                    .days
                    .entry(key)
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
                    });
                }
                changed = true;
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

/// sqlite 权威回填后的增量补齐：set_source_day 按天整体替换会抹掉 sqlite 尚未
    /// 落库的最新记录（rollout 偏移已消费、无法重加），因此从 recent 找回「超过该天
    /// sqlite 最大 completed_at」的记录重新累加回 days（只写 days，不重复写入 recent）。
    /// day_cutoffs：天 → 该天 sqlite 最大 completed_at(ms)；只处理出现在其中的天。
    /// 口径：recent 与 sqlite 侧同口径（含缓存 raw 累计），只回收超出权威时点的新记录。
    ///
    /// 幂等性：每次回填调用都会跑一次该函数（启动期 spawn + h3 每 5 分钟一次），
    /// 而 recent 不被本函数修改，单纯按 day_max 筛选 candidates 会让同一批
    /// "超出 day_max 的 recent" 在多次调用中被重复累加 → days 持续虚高。
    /// 改用 `recover_seen[source_id][day]` 记录「该天已累计过的最大 ts」，
    /// candidates 只接受 `r.ts > max(day_max, recover_seen)`，处理完后把当天水位推到
    /// 本轮处理到的最大 ts。set_source_day 整体替换某天会清掉该天水位（天数已重新落库，
    /// 旧水位失效）；重建/清源整源清零。
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
                        cost_partial: false,
                        calls: 0,
                        pct: 0,
                    });
                    mm.tokens += a.tokens;
                    mm.cost += a.cost;
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
}

// ---------- 聚合返回结构 ----------

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub tokens: f64,
    pub cost: f64,
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
                },
            ],
            true,
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

        // set_source_day 整体替换 days（含 SQLite 侧 30），同时清掉当天水位
        let mut models = BTreeMap::new();
        models.insert(
            "m".to_string(),
            DayModel {
                tokens: 30.0,
                input: 0.0,
                output: 0.0,
                cost: 0.0,
                calls: 2,
            },
        );
        store.set_source_day(sid, "1970-01-01", models);

        // 第二轮回填：day_max=2000，水位被 set_source_day 清零；
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
}
