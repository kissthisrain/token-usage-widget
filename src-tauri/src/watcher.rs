// 实时监听器：tail-f 式跟踪本地 JSONL 文件追加（对齐 core/watcher.js）
// - notify 事件（毫秒级）→ 读增量 → 解析 → 聚合 → 推送
// - 30s 轮询兜底（文件轮转/事件丢失）
// - 偏移量持久化在 history.fileOffsets，重启不重复统计
// - 自愈：3 分钟无进展且未消费 > 1MB → 重置偏移 + 清除近期数据 + 全量重扫

use crate::config::SourceConfig;
use crate::events::DataUpdate;
use crate::history::{HistoryStore, QuotaInfo, QuotaItem, RecordIn};
use crate::sources::local_logs::{
    extract_json_record, list_jsonl_files, parse_codex_file, parse_dsh_file, scan_params_for_source,
    ScanParams,
};
use crate::sqlite_import::import_zcode_history;
use crate::util::{now_ms, parse_ts};
use notify::{Event, RecursiveMode};
use notify::Watcher as _;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tokio::sync::mpsc;

const POLL_MS: u64 = 30000;
const STALL_MS: i64 = 3 * 60000;
const STALL_MIN_DELTA: u64 = 1024 * 1024;
/// sqlite 权威数据周期回填间隔（纠正 rollout 增量的瞬时误差）
const SQLITE_REIMPORT_MS: u64 = 5 * 60 * 1000;
/// 单文件解析大小上限（超出跳过，避免异常大文件拖垮/占满内存）
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub struct Watcher {
    inner: Arc<WatcherInner>,
}

struct WatcherInner {
    history: Arc<HistoryStore>,
    get_sources: Arc<dyn Fn() -> Vec<SourceConfig> + Send + Sync>,
    app: tauri::AppHandle,
    watchers: Mutex<HashMap<String, notify::RecommendedWatcher>>,
    scanning: Mutex<HashSet<String>>,
    last_advance: Mutex<HashMap<String, i64>>,
    event_tx: Mutex<Option<mpsc::UnboundedSender<Result<Event, notify::Error>>>>,
    stopped: AtomicBool,
    /// 后台循环任务句柄（stop/restart 时 abort，避免泄漏旧任务双倍轮询）
    tasks: Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>,
    /// 事件去抖：300ms 内同一源的高频事件合并为一次扫描
    pending_scans: Mutex<HashSet<String>>,
    /// 配置代际：restart 时 +1，异步扫描任务启动时捕获，代际不符则放弃（防旧配置扫描残留）
    epoch: std::sync::atomic::AtomicU64,
    /// 轮询阻塞任务是否在跑（防 30s 间隔叠加排队）
    poll_inflight: std::sync::atomic::AtomicBool,
    /// 重建（清空重扫）进行中的源集合：scan_source 与 rebuild_source 互斥，防并发双计
    rebuilding: Mutex<HashSet<String>>,
}

impl Watcher {
    pub fn new(
        history: Arc<HistoryStore>,
        get_sources: Arc<dyn Fn() -> Vec<SourceConfig> + Send + Sync>,
        app: tauri::AppHandle,
    ) -> Self {
        Self {
            inner: Arc::new(WatcherInner {
                history,
                get_sources,
                app,
                watchers: Mutex::new(HashMap::new()),
                scanning: Mutex::new(HashSet::new()),
                last_advance: Mutex::new(HashMap::new()),
                event_tx: Mutex::new(None),
                stopped: AtomicBool::new(false),
                tasks: Mutex::new(Vec::new()),
                pending_scans: Mutex::new(HashSet::new()),
                epoch: std::sync::atomic::AtomicU64::new(0),
                poll_inflight: std::sync::atomic::AtomicBool::new(false),
                rebuilding: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// 启动事件消费循环 + 30s 轮询兜底
    pub fn start(&self) {
        let this = self.clone();
        let inner = self.inner.clone();
        let (tx, mut rx) = mpsc::unbounded_channel::<Result<Event, notify::Error>>();
        *inner.event_tx.lock().unwrap() = Some(tx);
        // 事件消费循环
        let inner1 = inner.clone();
        let h1 = tauri::async_runtime::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if inner1.stopped.load(Ordering::SeqCst) {
                    break;
                }
                match ev {
                    Ok(ev) => this.handle_event(&ev),
                    Err(e) => log::warn!("[watcher] notify 事件错误: {e}"),
                }
            }
        });
        // 30s 轮询兜底 + 自愈（文件 IO 放到 spawn_blocking，避免阻塞 async worker）
        let this2 = self.clone();
        let h2 = tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(POLL_MS));
            interval.tick().await; // 首次立即
            loop {
                if this2.inner.stopped.load(Ordering::SeqCst) {
                    break;
                }
                this2.poll_all_blocking();
                interval.tick().await;
            }
        });
        // 每 5 分钟用 sqlite 权威数据整体回填（幂等），纠正 rollout 增量误差
        let inner4 = inner.clone();
        let h3 = tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(SQLITE_REIMPORT_MS));
            interval.tick().await; // 首次立即
            loop {
                if inner4.stopped.load(Ordering::SeqCst) {
                    break;
                }
                inner4.reimport_sqlite();
                interval.tick().await;
            }
        });
        inner.tasks.lock().unwrap().extend([h1, h2, h3]);
    }

    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.watchers.lock().unwrap().clear();
        // abort 后台循环任务，避免 restart 后旧任务残留（双倍轮询/事件丢失）
        for h in self.inner.tasks.lock().unwrap().drain(..) {
            h.abort();
        }
        self.inner.pending_scans.lock().unwrap().clear();
    }

    /// 重建监听（配置变更后调用）
    pub fn restart(&self) {
        self.stop();
        self.inner.stopped.store(false, Ordering::SeqCst);
        // 代际 +1：让上一代异步扫描任务（sleep 去抖/初始扫描）失效，避免旧配置残留
        self.inner.epoch.fetch_add(1, Ordering::SeqCst);
        self.start();
        let sources = (self.inner.get_sources)();
        for src in sources {
            self.inner.watch_source(&src);
            // 初始扫描（含增量）只针对启用的本地记录源；Watcher 可克隆，放 spawn_blocking
            // 避免阻塞调用线程（重启/保存配置路径），并带 epoch 检查防止旧配置扫描残留
            if src.type_ != "local-logs" || !src.enabled {
                continue;
            }
            let params = scan_params_for_source(&src);
            let epoch = self.inner.epoch.load(Ordering::SeqCst);
            let this = self.clone();
            let src0 = src.clone();
            let params0 = params.clone();
            tauri::async_runtime::spawn(async move {
                let this2 = this.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if this2.inner.epoch.load(Ordering::SeqCst) != epoch {
                        return; // 配置已变更，放弃旧扫描
                    }
                    this2.inner.scan_source(&src0, &params0);
                })
                .await;
            });
        }
    }

    /// 文件系统事件 → 命中的源做去抖扫描
    fn handle_event(&self, ev: &Event) {
        let inner = &self.inner;
        if inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        let sources = (inner.get_sources)();
        for src in &sources {
            if src.type_ != "local-logs" || !src.enabled {
                continue;
            }
            let Some(dir) = &src.path else { continue };
            let dirb = PathBuf::from(dir);
            let hit = ev.paths.iter().any(|p| p.starts_with(&dirb));
            if !hit {
                continue;
            }
            let params = scan_params_for_source(src);
            let name_hit = ev.paths.iter().any(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| params.pattern.is_match(n))
                    .unwrap_or(false)
            });
            let structural = matches!(
                ev.kind,
                notify::EventKind::Remove(_)
                    | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                    | notify::EventKind::Create(_)
            );
            if name_hit || structural {
                self.schedule_scan(src, &params);
            }
        }
    }

    /// 轮询兜底：文件 IO 放到 spawn_blocking（30s 间隔 + inflight 防抖，避免阻塞 async worker）
    fn poll_all_blocking(&self) {
        if self.inner.poll_inflight.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let this2 = this.clone();
            let _ = tokio::task::spawn_blocking(move || {
                this2.inner.poll_all();
            })
            .await;
            this.inner.poll_inflight.store(false, Ordering::SeqCst);
        });
    }

    /// 事件去抖：300ms 内同一源的高频事件（Codex 每 turn 追加多行）合并为一次扫描
    fn schedule_scan(&self, src: &SourceConfig, params: &ScanParams) {
        let mut pending = self.inner.pending_scans.lock().unwrap();
        if !pending.insert(src.id.clone()) {
            return;
        }
        drop(pending);
        let this = self.clone();
        let src = src.clone();
        let params = params.clone();
        let epoch = self.inner.epoch.load(Ordering::SeqCst);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            this.inner.pending_scans.lock().unwrap().remove(&src.id);
            if this.inner.stopped.load(Ordering::SeqCst)
                || this.inner.epoch.load(Ordering::SeqCst) != epoch
            {
                return;
            }
            let this2 = this.clone();
            let src2 = src.clone();
            let params2 = params.clone();
            let _ = tokio::task::spawn_blocking(move || {
                this2.inner.scan_source(&src2, &params2);
            })
            .await;
        });
    }
}

impl WatcherInner {
    fn watch_source(&self, src: &SourceConfig) {
        if src.type_ != "local-logs" || !src.enabled {
            return;
        }
        let Some(dir) = &src.path else { return };
        let dir = PathBuf::from(dir);
        if !dir.is_dir() {
            return;
        }
        let params = scan_params_for_source(src);
        {
            let mut watchers = self.watchers.lock().unwrap();
            if watchers.contains_key(&src.id) {
                return;
            }
            let mode = if params.recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            if let Some(tx) = self.event_tx.lock().unwrap().as_ref() {
                let tx = tx.clone();
                match notify::recommended_watcher(move |res| {
                    let _ = tx.send(res);
                }) {
                    Ok(mut w) => {
                        if w.watch(&dir, mode).is_ok() {
                            watchers.insert(src.id.clone(), w);
                        }
                    }
                    Err(e) => log::warn!("[watcher] {} 监听失败: {}", src.name, e),
                }
            }
        }
        self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
        // 初始扫描（含增量）由 restart() 统一发起（Watcher 可克隆，spawn_blocking + epoch 检查）
        // ZCode 数据库历史回填（只回填今天之前，异步不阻塞）
        if src.tool.as_deref() == Some("zcode") && src.import_sqlite != Some(false) {
            let src = src.clone();
            let history = self.history.clone();
            let app = self.app.clone();
            tauri::async_runtime::spawn(async move {
                let r = import_zcode_history(&src, history).await;
                if r.days > 0 {
                    let _ = app.emit(
                        "data:update",
                        DataUpdate {
                            source_id: Some(src.id.clone()),
                            count: Some(r.imported),
                            ..Default::default()
                        },
                    );
                }
            });
        }
    }

    fn poll_all(&self) {
        let sources = (self.get_sources)();
        for src in &sources {
            if src.type_ != "local-logs" || !src.enabled {
                continue;
            }
            let Some(dir) = &src.path else { continue };
            if !PathBuf::from(dir).is_dir() {
                continue;
            }
            let params = scan_params_for_source(src);
            self.scan_source(src, &params);
            self.heal_if_stalled(src, &params);
        }
    }

    /// 自愈：长时间未消费且文件持续增长 → 重置偏移 + 清除近期数据 + 全量重扫
    fn heal_if_stalled(&self, src: &SourceConfig, params: &ScanParams) {
        let last = self
            .last_advance
            .lock()
            .unwrap()
            .get(&src.id)
            .copied()
            .unwrap_or(0);
        if now_ms() - last < STALL_MS {
            return;
        }
        let dir = PathBuf::from(src.path.clone().unwrap_or_default());
        let files = list_jsonl_files(&dir, params.recursive, &params.pattern);
        let mut total_size: u64 = 0;
        let mut consumed: u64 = 0;
        for f in &files {
            total_size += std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            consumed += self.history.get_file_offset(&src.id, &f.to_string_lossy());
        }
        if total_size.saturating_sub(consumed) < STALL_MIN_DELTA {
            self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
            return;
        }
        // 清数据前验证文件可读：文件打不开（独占锁/网络盘断连）时，
        // 清了历史也补不回来 → 只跳过本轮，下轮重试
        let readable = files.iter().any(|f| std::fs::File::open(f).is_ok());
        if !readable {
            log::warn!(
                "[watcher] {} 文件当前不可读，暂不清数据（下轮重试）",
                src.name
            );
            self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
            return;
        }
        log::warn!(
            "[watcher] {} 疑似卡死（{}KB 未消费），重置偏移全量重扫…",
            src.name,
            total_size.saturating_sub(consumed) / 1024
        );
        // 清数据前备份快照（误判时可用 history.json.pre-heal.bak 手工恢复）
        self.history.backup_before_heal();
        if self.rebuild_source(src, params) {
            // 只有本次调用真正执行了重建才做收尾；提前返回（已有重建在进行）时
            // 由发起方负责回填与通知，避免重复回填/误导性 toast
            self.after_rebuild(src);
        }
    }

    /// 重建某源：重置全部文件偏移 + 清空该源全部 days/recent + 全量重扫。
    /// 返回 true 表示本次调用实际执行了重建；false 表示该源已在重建中（跳过，
    /// 由发起方负责收尾）。按源标记 rebuilding，与 scan_source 互斥防并发双计。
    fn rebuild_source(&self, src: &SourceConfig, params: &ScanParams) -> bool {
        if !self.rebuilding.lock().unwrap().insert(src.id.clone()) {
            return false; // 该源已在重建中（嵌套触发），跳过
        }
        let dir = PathBuf::from(src.path.clone().unwrap_or_default());
        let files = list_jsonl_files(&dir, params.recursive, &params.pattern);
        let file_strs: Vec<String> = files.iter().map(|f| f.to_string_lossy().to_string()).collect();
        self.history.reset_file_offsets(&src.id, &file_strs);
        // dsh 的事件计数同样重置（全量重扫重新计数，防旧计数导致漏计）
        self.history.reset_dsh_seen(&src.id);
        // 必须清干净再重扫：旧记录若保留，重扫会把它们再累加一遍（双倍计数）
        self.history.clear_source_all(&src.id);
        for f in &files {
            self.read_delta(src, params, f);
        }
        self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
        self.rebuilding.lock().unwrap().remove(&src.id);
        true
    }

    /// 重建成功后的收尾：zcode 源异步 sqlite 回填 + 通知前端（自愈提示）
    fn after_rebuild(&self, src: &SourceConfig) {
        if src.tool.as_deref() == Some("zcode") && src.import_sqlite != Some(false) {
            let src = src.clone();
            let history = self.history.clone();
            tauri::async_runtime::spawn(async move {
                let _ = import_zcode_history(&src, history).await;
            });
        }
        let _ = self.app.emit(
            "data:update",
            DataUpdate {
                source_id: Some(src.id.clone()),
                count: Some(0),
                healed: Some(true),
                ..Default::default()
            },
        );
    }

    fn scan_source(&self, src: &SourceConfig, params: &ScanParams) {
        if self.scanning.lock().unwrap().contains(&src.id)
            || self.rebuilding.lock().unwrap().contains(&src.id)
        {
            return;
        }
        self.scanning.lock().unwrap().insert(src.id.clone());
        let dir = PathBuf::from(src.path.clone().unwrap_or_default());
        let files = list_jsonl_files(&dir, params.recursive, &params.pattern);
        for f in files {
            self.read_delta(src, params, &f);
        }
        self.scanning.lock().unwrap().remove(&src.id);
    }

    fn read_delta(&self, src: &SourceConfig, params: &ScanParams, file: &std::path::Path) {
        let size = match std::fs::metadata(file) {
            Ok(m) => m.len(),
            Err(_) => return,
        };
        if size == 0 {
            return;
        }
        let off = self.history.get_file_offset(&src.id, &file.to_string_lossy());
        // 文件被截断/重写（offset 越过当前大小）：直接重建该源，防止整文件重加导致双倍计数
        if off > size {
            log::warn!(
                "[watcher] {} 检测到文件被截断/重写（off={} > size={}），重建该源数据…",
                src.name,
                off,
                size
            );
            self.history.backup_before_heal();
            if self.rebuild_source(src, params) {
                // 仅当本次调用真正执行了重建才做收尾（避免重复回填/误导性 toast）
                self.after_rebuild(src);
            }
            return;
        }
        if off == size {
            return;
        }

        // codex 文件整体有序解析 → 全量重读（解析依赖上下文，如 turn_context 决定 model），
        // 但只累加「内容末尾偏移 > 已消费 offset」的增量记录，重复事件/轮询不会重复累计
        if params.format == "codex" {
            if size > MAX_FILE_BYTES {
                // 跳过超大文件，但标记为已消费：否则每次轮询重试 + 触发反复自愈（清空重扫）
                log::warn!(
                    "[watcher] {} 文件超过 {}MB 跳过解析（已标记消费，新追加内容仍会统计）：{}",
                    src.name,
                    MAX_FILE_BYTES / 1024 / 1024,
                    file.display()
                );
                self.history.set_file_offset(&src.id, &file.to_string_lossy(), size);
                self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
                return;
            }
            let (records, rate_limits) = parse_codex_file(file);
            let delta: Vec<RecordIn> = records
                .into_iter()
                .filter(|(_, end_off)| *end_off > off)
                .map(|(r, _)| r)
                .collect();
            if !delta.is_empty() {
                let n = delta.len() as u64;
                self.history.add_records(&src.id, delta, true);
                let _ = self.app.emit(
                    "data:update",
                    DataUpdate {
                        source_id: Some(src.id.clone()),
                        count: Some(n),
                        ..Default::default()
                    },
                );
            }
            if !rate_limits.is_empty() {
                let last = rate_limits.last().cloned().unwrap();
                let item = QuotaItem {
                    label: last.label.clone(),
                    ..last
                };
                self.history.set_quota(
                    &src.id,
                    QuotaInfo {
                        source_id: Some(src.id.clone()),
                        items: vec![item],
                        mode: Some("local-rate-limits".to_string()),
                        local: Some(true),
                        at: now_ms(),
                        ..Default::default()
                    },
                );
                let _ = self.app.emit(
                    "data:update",
                    DataUpdate {
                        source_id: Some(src.id.clone()),
                        ..Default::default()
                    },
                );
            }
            self.history.set_file_offset(&src.id, &file.to_string_lossy(), size);
            self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
            return;
        }

        // dsh：zstd 压缩 JSONL，无法按字节偏移增量读 → 全量解压重读，
        // 用「已消费事件数」取尾部增量（文件只追加、新事件总在尾部），防重启/重复事件双计
        if params.format == "dsh" {
            if size > MAX_FILE_BYTES {
                // 跳过超大文件并标记消费：避免每次轮询重试 + 触发反复自愈（同 codex 路径）
                log::warn!(
                    "[watcher] {} 文件超过 {}MB 跳过解析（已标记消费，新追加内容仍会统计）：{}",
                    src.name,
                    MAX_FILE_BYTES / 1024 / 1024,
                    file.display()
                );
                self.history.set_file_offset(&src.id, &file.to_string_lossy(), size);
                self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
                return;
            }
            let Some(records) = parse_dsh_file(file) else {
                // 解压失败（写入中/损坏）：只刷新 last_advance 防自愈误触发，
                // 不推进 seen/offset —— 否则下次成功解析会整文件重加 → 双计
                self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
                return;
            };
            let total = records.len() as u64;
            let seen = self.history.get_dsh_seen(&src.id, &file.to_string_lossy());
            let new_count = total.saturating_sub(seen);
            if new_count > 0 {
                let start = records.len() - new_count as usize;
                let new_records: Vec<RecordIn> = records[start..].to_vec();
                self.history.add_records(&src.id, new_records, true);
                let _ = self.app.emit(
                    "data:update",
                    DataUpdate {
                        source_id: Some(src.id.clone()),
                        count: Some(new_count),
                        ..Default::default()
                    },
                );
            }
            self.history.set_dsh_seen(&src.id, &file.to_string_lossy(), total);
            self.history.set_file_offset(&src.id, &file.to_string_lossy(), size);
            self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
            return;
        }

        // zcode：只读增量（多行安全解析：花括号配对提取，损坏片段跳过，尾部未写完留待下次）
        let buf: Vec<u8> = match std::fs::File::open(file) {
            Ok(mut f) => {
                use std::io::{Read, Seek, SeekFrom};
                if f.seek(SeekFrom::Start(off)).is_err() {
                    return;
                }
                let mut b = vec![0u8; (size - off) as usize];
                if f.read_exact(&mut b).is_err() {
                    return; // 读取中途被截断，保持偏移不动，下轮处理
                }
                b
            }
            Err(_) => return,
        };
        let mut records: Vec<RecordIn> = Vec::new();
        let mut pos: usize = 0;
        while pos < buf.len() {
            let (rs, re) = match extract_json_record(&buf, pos) {
                Some(r) => r,
                None => break,
            };
            let seg = String::from_utf8_lossy(&buf[rs..re]);
            match serde_json::from_str::<serde_json::Value>(&seg) {
                Ok(v) => {
                    if let Some(p) = parse_zcode_value(&v) {
                        records.push(p);
                    }
                    pos = re;
                }
                Err(_) => {
                    pos = rs + 1;
                    continue;
                }
            }
        }
        if !records.is_empty() {
            let n = records.len() as u64;
            self.history.add_records(&src.id, records, true);
            let _ = self.app.emit(
                "data:update",
                DataUpdate {
                    source_id: Some(src.id.clone()),
                    count: Some(n),
                    ..Default::default()
                },
            );
        }
        let end_offset = (off + pos as u64).min(size);
        if end_offset > off {
            self.history.set_file_offset(&src.id, &file.to_string_lossy(), end_offset);
            self.last_advance.lock().unwrap().insert(src.id.clone(), now_ms());
        }
    }
    /// 对所有启用且允许导入的 zcode 源执行 sqlite 回填（spawn_blocking）
    fn reimport_sqlite(&self) {
        let sources = (self.get_sources)();
        for src in &sources {
            if src.type_ != "local-logs" || !src.enabled {
                continue;
            }
            if src.tool.as_deref() != Some("zcode") || src.import_sqlite == Some(false) {
                continue;
            }
            let src = src.clone();
            let history = self.history.clone();
            let app = self.app.clone();
            tauri::async_runtime::spawn(async move {
                let r = import_zcode_history(&src, history).await;
                if r.days > 0 {
                    let _ = app.emit(
                        "data:update",
                        DataUpdate {
                            source_id: Some(src.id.clone()),
                            count: Some(r.imported),
                            ..Default::default()
                        },
                    );
                }
            });
        }
    }
}

/// 与 local_logs 的 zcode 解析保持一致（watcher 中独立解析，避免反转依赖）
fn parse_zcode_value(r: &serde_json::Value) -> Option<RecordIn> {
    if !r.is_object() {
        return None;
    }
    let u = r.get("response").and_then(|x| x.get("usage"));
    let total = u
        .and_then(|x| x.get("totalTokens").or_else(|| x.get("total_tokens")))
        .and_then(serde_json::Value::as_f64)
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
        .and_then(serde_json::Value::as_str)
        .map(|s| s.to_string());
    let num = |k1: &str, k2: &str| {
        u.and_then(|x| x.get(k1).or_else(|| x.get(k2)))
            .and_then(serde_json::Value::as_f64)
    };
    Some(RecordIn {
        ts,
        model,
        provider: None,
        input: num("inputTokens", "input_tokens"),
        output: num("outputTokens", "output_tokens"),
        total,
        cache_read: num("cacheReadTokens", "cached_input_tokens"),
        cost: None,
    })
}
