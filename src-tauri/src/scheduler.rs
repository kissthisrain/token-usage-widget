// 定时调度器：周期性拉取 API 类数据源（对齐 core/scheduler.js）
// - openai: 拉取按天用量 → 幂等替换历史（不重复累计）
// - compat: 余额探测 → 写配额缓存
// - opencode-go 官方抓取在二期（一期仅本地估算）

use crate::config::{ConfigStore, SourceConfig};
use crate::events::DataUpdate;
use crate::history::{DayModel, HistoryStore};
use crate::sources::{compat, openai};
use crate::util::{date_key, now_ms};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

pub struct Scheduler {
    config: Arc<ConfigStore>,
    history: Arc<HistoryStore>,
    app: tauri::AppHandle,
    running: AtomicBool,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl Scheduler {
    pub fn new(
        config: Arc<ConfigStore>,
        history: Arc<HistoryStore>,
        app: tauri::AppHandle,
    ) -> Self {
        Self {
            config,
            history,
            app,
            running: AtomicBool::new(false),
            task: Mutex::new(None),
        }
    }

    pub fn start(&self) {
        self.stop();
        // 循环体放在 SchedulerFork（共享引用），handle 存于 self.task 供 stop abort
        let this_arc = Arc::new(SchedulerFork {
            config: self.config.clone(),
            history: self.history.clone(),
            app: self.app.clone(),
            running: Arc::new(AtomicBool::new(false)),
        });
        let min = self
            .config
            .get_general()
            .refresh_interval_min
            .max(1);
        let handle = tauri::async_runtime::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs((min as u64) * 60));
            interval.tick().await; // 消费首次立即触发
            // 启动 1.5s 后首拉（对齐 JS setTimeout 1500）
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            SchedulerFork::tick(&this_arc).await;
            loop {
                interval.tick().await;
                SchedulerFork::tick(&this_arc).await;
            }
        });
        *self.task.lock().unwrap() = Some(handle);
        // 同步运行标志（仅防重入，跨 fork 意义不大，保留兼容）
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        if let Some(h) = self.task.lock().unwrap().take() {
            h.abort();
        }
    }

    /// 立即刷新全部源（设置界面「立即刷新」）
    pub async fn tick(&self) {
        // 复用 SchedulerFork 的 tick 逻辑
        let fork = SchedulerFork {
            config: self.config.clone(),
            history: self.history.clone(),
            app: self.app.clone(),
            running: Arc::new(AtomicBool::new(false)),
        };
        SchedulerFork::tick(&fork).await;
    }
}

struct SchedulerFork {
    config: Arc<ConfigStore>,
    history: Arc<HistoryStore>,
    app: tauri::AppHandle,
    running: Arc<AtomicBool>,
}

impl SchedulerFork {
    async fn tick(this: &SchedulerFork) {
        if this.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let sources = this.config.get_sources(true);
        for src in sources {
            let r = Self::run_for_source(this, &src).await;
            if let Err(e) = r {
                log::warn!("[scheduler] {}: {}", src.name, e);
                if src.type_ == "openai" {
                    this.history.set_quota(
                        &src.id,
                        crate::history::QuotaInfo {
                            source_id: Some(src.id.clone()),
                            mode: Some("openai".to_string()),
                            items: vec![],
                            ok: Some(false),
                            error: Some(e),
                            at: now_ms(),
                            ..Default::default()
                        },
                    );
                    let _ = this.app.emit(
                        "data:update",
                        DataUpdate {
                            source_id: Some(src.id.clone()),
                            ..Default::default()
                        },
                    );
                }
            }
        }
        this.running.store(false, Ordering::SeqCst);
    }

    async fn run_for_source(
        this: &SchedulerFork,
        src: &SourceConfig,
    ) -> Result<(), String> {
        match src.type_.as_str() {
            "openai" => {
                let records = openai::fetch_usage(src, 90).await?;
                // 按天分桶 → 幂等替换
                let mut by_day: BTreeMap<String, BTreeMap<String, DayModel>> = BTreeMap::new();
                for r in &records {
                    let k = date_key(r.ts.unwrap_or_else(now_ms));
                    let model = r.model.clone().unwrap_or_else(|| "(unknown)".to_string());
                    let a = by_day
                        .entry(k)
                        .or_default()
                        .entry(model)
                        .or_insert_with(|| DayModel {
                            tokens: 0.0,
                            input: 0.0,
                            output: 0.0,
                            cost: 0.0,
                            credits: 0.0,
                            calls: 0,
                        });
                    a.tokens += r.total;
                    a.input += r.input.unwrap_or(0.0);
                    a.output += r.output.unwrap_or(0.0);
                    if let Some(c) = r.cost {
                        a.cost += c;
                    }
                    a.calls += 1;
                }
                for (k, models) in &by_day {
                    this.history.set_source_day(&src.id, k, models.clone());
                }
                this.history.set_quota(
                    &src.id,
                    crate::history::QuotaInfo {
                        source_id: Some(src.id.clone()),
                        mode: Some("openai-usage".to_string()),
                        items: vec![],
                        ok: Some(true),
                        at: now_ms(),
                        ..Default::default()
                    },
                );
                log::info!("[scheduler] {}: 用量更新 {} 条", src.name, records.len());
                let _ = this.app.emit(
                    "data:update",
                    DataUpdate {
                        source_id: Some(src.id.clone()),
                        ..Default::default()
                    },
                );
                Ok(())
            }
            "compat" => {
                let r = compat::probe(src).await;
                if r.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                    let mode = r
                        .get("mode")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let items: Vec<crate::history::QuotaItem> =
                        serde_json::from_value(r.get("items").cloned().unwrap_or(serde_json::json!([])))
                            .unwrap_or_default();
                    this.history.set_quota(
                        &src.id,
                        crate::history::QuotaInfo {
                            source_id: Some(src.id.clone()),
                            mode: Some(mode),
                            items,
                            ok: Some(true),
                            at: now_ms(),
                            ..Default::default()
                        },
                    );
                    log::info!("[scheduler] {}: 余额探测成功", src.name);
                } else {
                    let err = r
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("未知错误")
                        .to_string();
                    this.history.set_quota(
                        &src.id,
                        crate::history::QuotaInfo {
                            source_id: Some(src.id.clone()),
                            mode: None,
                            items: vec![],
                            ok: Some(false),
                            error: Some(err.clone()),
                            at: now_ms(),
                            ..Default::default()
                        },
                    );
                    log::warn!("[scheduler] {}: {}", src.name, err);
                }
                let _ = this.app.emit(
                    "data:update",
                    DataUpdate {
                        source_id: Some(src.id.clone()),
                        ..Default::default()
                    },
                );
                Ok(())
            }
            // opencode-go 官方额度源：周期抓取官方用量（未配置 Cookie 时给出错误提示；
            // 本地代理源的估算配额由 snapshot 动态计算，不在此处理）
            "opencode-go-official" => {
                let r = crate::sources::opencode_go::probe_quota(src).await;
                if r.ok {
                    this.history.set_quota(
                        &src.id,
                        crate::history::QuotaInfo {
                            source_id: Some(src.id.clone()),
                            mode: Some("opencode-go-official".to_string()),
                            items: r.items.clone(),
                            ok: Some(true),
                            currency: Some("USD".to_string()),
                            official: Some(true),
                            at: now_ms(),
                            ..Default::default()
                        },
                    );
                    log::info!(
                        "[scheduler] {}: 官方用量更新 ({})",
                        src.name,
                        r.items
                            .iter()
                            .map(|i| format!(
                                "{} {:.1}%",
                                i.key.clone().unwrap_or_default(),
                                i.used_pct.unwrap_or(0.0)
                            ))
                            .collect::<Vec<_>>()
                            .join("/")
                    );
                } else {
                    // 保留上次成功数据，仅附加错误
                    let prev = this.history.data().quota.get(&src.id).cloned();
                    this.history.set_quota(
                        &src.id,
                        crate::history::QuotaInfo {
                            source_id: Some(src.id.clone()),
                            mode: Some("opencode-go-official".to_string()),
                            items: prev.map(|q| q.items).unwrap_or_default(),
                            ok: Some(false),
                            error: r.error.clone(),
                            currency: Some("USD".to_string()),
                            official: Some(true),
                            at: now_ms(),
                            ..Default::default()
                        },
                    );
                    log::warn!(
                        "[scheduler] {}: {}",
                        src.name,
                        r.error.clone().unwrap_or_default()
                    );
                }
                let _ = this.app.emit(
                    "data:update",
                    DataUpdate {
                        source_id: Some(src.id.clone()),
                        ..Default::default()
                    },
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
