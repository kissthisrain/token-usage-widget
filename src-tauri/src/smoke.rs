// 冒烟测试：--smoke 无界面验证数据管道（对齐 Electron runSmoke）
// 用法：cargo run -- --smoke  （或打包后的 exe --smoke）

use crate::config::{ConfigStore, SourceConfig};
use crate::detect;
use crate::history::HistoryStore;
use crate::sources::local_logs::{list_jsonl_files, parse_file, scan_params_for_source};
use crate::util::now_ms;

pub fn run() {
    println!("=== 环境 ===");
    println!("userData: {}", crate::config::user_data_dir().display());
    let config = ConfigStore::new(&crate::config::user_data_dir());
    config.load();
    let cfg = config.get();
    println!(
        "config 数据源数: {} {:?}",
        cfg.sources.len(),
        cfg.sources.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
    );

    let detections = detect::scan_all();
    println!("=== 工具探测 ===");
    for d in &detections {
        println!("{d:?}");
    }

    // 临时源：用探测到的工具补全快照
    let mut temp_sources: Vec<SourceConfig> = Vec::new();
    if cfg.sources.is_empty() {
        for d in &detections {
            if d.exists && d.record_count.unwrap_or(0) > 0 {
                temp_sources.push(SourceConfig {
                    id: format!("smoke_{}", d.id),
                    type_: "local-logs".to_string(),
                    tool: Some(d.tool.clone()),
                    name: d.name.clone(),
                    path: d.path.clone(),
                    enabled: true,
                    ..Default::default()
                });
            }
        }
    }
    let sources: Vec<SourceConfig> = if temp_sources.is_empty() {
        cfg.sources.clone()
    } else {
        temp_sources
    };

    // 解析全部本地文件（模拟 watcher 初始扫描）
    // 注意：使用内存模式 HistoryStore，绝不落盘，避免污染真实 history.json
    let history = std::sync::Arc::new(HistoryStore::new_memory());
    for s in &sources {
        if s.type_ != "local-logs" {
            continue;
        }
        let Some(dir) = &s.path else { continue };
        let params = scan_params_for_source(s);
        let files = list_jsonl_files(std::path::Path::new(dir), params.recursive, &params.pattern);
        let mut n = 0usize;
        let mut rate = 0usize;
        for f in &files {
            let (records, rate_limits) = parse_file(f, &params.format);
            if !records.is_empty() {
                let count = records.len();
                history.add_records(&s.id, records, true);
                n += count;
            }
            if !rate_limits.is_empty() {
                rate += rate_limits.len();
                let last = rate_limits.last().cloned().unwrap();
                let item = crate::history::QuotaItem {
                    label: last.label.clone(),
                    ..last
                };
                history.set_quota(
                    &s.id,
                    crate::history::QuotaInfo {
                        source_id: Some(s.id.clone()),
                        items: vec![item],
                        mode: Some("local-rate-limits".to_string()),
                        local: Some(true),
                        at: now_ms(),
                        ..Default::default()
                    },
                );
            }
        }
        println!("[smoke] {}: {} 文件, {} 条用量记录, {} 条配额", s.name, files.len(), n, rate);
    }

    let snap = crate::snapshot::build_snapshot_with_sources(&sources, &history, "month");
    println!("=== 快照 ===");
    let summary = serde_json::json!({
        "totals": snap.totals,
        "activeDays": snap.active_days,
        "modelCount": snap.models.len(),
        "topModels": snap.models.iter().take(5).collect::<Vec<_>>(),
        "trendDays": snap.trend.len(),
        "heatmapDays": snap.heatmap.len(),
        "sources": snap.sources.iter().map(|s| serde_json::json!({ "id": s.id, "name": s.name, "tokens": s.tokens })).collect::<Vec<_>>(),
        "quota": snap.quota.iter().map(|q| serde_json::json!({ "mode": q.mode, "items": q.items.iter().map(|i| serde_json::json!({ "label": i.label, "usedPct": i.used_pct, "resetsAt": i.resets_at })).collect::<Vec<_>>() })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
    std::process::exit(0);
}
