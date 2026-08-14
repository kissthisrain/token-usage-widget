// ZCode 数据库历史导入：只读 ~/.zcode/cli/db/db.sqlite 的 model_usage 表，回填用量
// - db.sqlite 是权威数据源（rollout JSONL 只覆盖部分请求，实测 728/184 条），因此导入**全部**记录（含今天）
// - 按天幂等替换（setSourceDay），重复运行安全
// - watcher 启动时 + 周期任务调用，周期覆盖可纠正 rollout 增量的瞬时误差

use crate::config::SourceConfig;
use crate::history::{DayModel, HistoryStore};
use crate::util::date_key;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn default_zcode_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zcode")
        .join("cli")
        .join("db")
        .join("db.sqlite")
}

pub struct ImportResult {
    pub imported: u64,
    pub days: usize,
    #[allow(dead_code)]
    pub skipped: bool,
    #[allow(dead_code)]
    pub error: Option<String>,
}

/// 阻塞式导入（调用方放到 spawn_blocking）
fn import_blocking(
    source: &SourceConfig,
    history: &HistoryStore,
    db_path: PathBuf,
) -> ImportResult {
    if !db_path.exists() {
        return ImportResult { imported: 0, days: 0, skipped: true, error: None };
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            return ImportResult {
                imported: 0,
                days: 0,
                skipped: false,
                error: Some(format!("打开数据库失败: {e}")),
            }
        }
    };
    // 导入全部记录（含今天）：sqlite 为权威数据源，rollout 文件不完整
    let sql = "SELECT completed_at, model_id, input_tokens, output_tokens, computed_total_tokens \
               FROM model_usage WHERE completed_at IS NOT NULL";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            return ImportResult {
                imported: 0,
                days: 0,
                skipped: false,
                error: Some(format!("查询失败: {e}")),
            }
        }
    };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, f64>(4)?,
        ))
    }) {
        Ok(r) => r,
        Err(e) => {
            return ImportResult {
                imported: 0,
                days: 0,
                skipped: false,
                error: Some(format!("查询失败: {e}")),
            }
        }
    };

    let mut by_day: BTreeMap<String, BTreeMap<String, DayModel>> = BTreeMap::new();
    for r in rows.flatten() {
        let (completed_at, model_id, input_tokens, output_tokens, computed_total) = r;
        let total = computed_total;
        if !total.is_finite() || total <= 0.0 {
            continue;
        }
        let day = date_key(completed_at);
        let model = model_id.unwrap_or_else(|| "(unknown)".to_string());
        let a = by_day
            .entry(day)
            .or_default()
            .entry(model)
            .or_insert_with(|| DayModel {
                tokens: 0.0,
                input: 0.0,
                output: 0.0,
                cost: 0.0,
                calls: 0,
            });
        a.tokens += total;
        a.input += input_tokens.unwrap_or(0.0);
        a.output += output_tokens.unwrap_or(0.0);
        a.calls += 1;
    }

    let mut imported: u64 = 0;
    for (day, models) in by_day.iter() {
        let calls: u64 = models.values().map(|a| a.calls).sum();
        // 合并而非整体替换：rollout 实时增量可能包含 sqlite 尚未落库的最新请求，
        // 整体替换会抹掉它们（偏移已消费无法重加）；按模型取 max 双向都安全（幂等）
        history.merge_source_day(&source.id, day, models.clone());
        imported += calls;
    }
    if imported > 0 {
        log::info!("[sqlite] {}: 回填 {} 条历史（{} 天）", source.name, imported, by_day.len());
    }
    ImportResult { imported, days: by_day.len(), skipped: false, error: None }
}

/// 异步包装：spawn_blocking 避免阻塞主线程
pub async fn import_zcode_history(
    source: &SourceConfig,
    history: std::sync::Arc<HistoryStore>,
) -> ImportResult {
    let db_path = source
        .sqlite_db
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_zcode_db_path);
    let src = source.clone();
    tokio::task::spawn_blocking(move || import_blocking(&src, &history, db_path))
        .await
        .unwrap_or_else(|e| ImportResult {
            imported: 0,
            days: 0,
            skipped: false,
            error: Some(format!("任务失败: {e}")),
        })
}
