// 快照构建：对齐 core/snapshot.js（渲染进程统一数据视图）

use crate::config::{ConfigStore, SourceConfig};
use crate::history::{HistoryStore, ModelAgg, QuotaInfo};
use crate::sources::opencode_go::compute_quota;
use crate::util::now_ms;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourceAgg {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub enabled: bool,
    pub tokens: f64,
    pub cost: f64,
    pub cost_partial: bool,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourcesMeta {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub enabled: bool,
    #[serde(default)]
    pub display_order: i64,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotGeneral {
    pub show_cost: bool,
    pub show_quota: bool,
    pub show_charts: bool,
    pub show_model_list: bool,
    pub opacity: f64,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub range: String,
    pub source_filter: String,
    pub totals: crate::history::Totals,
    pub models: Vec<ModelAgg>,
    pub trend: Vec<crate::history::TrendPoint>,
    pub heatmap: Vec<crate::history::HeatmapPoint>,
    pub active_days: usize,
    pub sources: Vec<SourceAgg>,
    pub quota: Vec<QuotaInfo>,
    pub sources_meta: Vec<SourcesMeta>,
    pub general: SnapshotGeneral,
    pub updated_at: i64,
    pub now: i64,
}

pub fn build_snapshot(config: &ConfigStore, history: &HistoryStore, range: &str, source_id: &str) -> Snapshot {
    let enabled = config.get_sources(true);
    let ids: Vec<String> = enabled.iter().map(|s| s.id.clone()).collect();

    let source_ids: Vec<String> = if source_id != "all" && ids.contains(&source_id.to_string()) {
        vec![source_id.to_string()]
    } else {
        ids.clone()
    };

    // 模型白名单
    let mut whitelist: HashMap<String, HashSet<String>> = HashMap::new();
    for s in &enabled {
        if !s.model_whitelist.is_empty() {
            whitelist.insert(s.id.clone(), s.model_whitelist.iter().cloned().collect());
        }
    }

    let agg = history.aggregate(&source_ids, range, &whitelist);

    // 每个启用的源：独立聚合
    let sources: Vec<SourceAgg> = enabled
        .iter()
        .map(|s| {
            let a = history.aggregate(std::slice::from_ref(&s.id), range, &whitelist);
            SourceAgg {
                id: s.id.clone(),
                name: s.name.clone(),
                type_: s.type_.clone(),
                enabled: s.enabled,
                tokens: a.totals.tokens,
                cost: a.totals.cost,
                cost_partial: a.totals.cost_partial,
            }
        })
        .collect();

    // opencode-go 配额：
    // - 配置官方查询（工作区 ID + Cookie）→ 用缓存的官方数据（权威），缓存缺失/无数据且无错误时本地估算兜底
    // - 未配置 → 每次快照本地动态计算
    for s in &enabled {
        if s.type_ != "opencode-go" {
            continue;
        }
        let q = history.data().quota.get(&s.id).cloned();
        let needs_local = match &q {
            None => true,
            Some(qi) => qi.items.is_empty() && qi.error.is_none(),
        };
        let is_official_configured = s.workspace_id.is_some() && s.auth_cookie.is_some();
        if !is_official_configured || needs_local {
            history.set_quota(&s.id, compute_quota(history, s));
        }
    }

    let name_by_id: HashMap<String, String> =
        enabled.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
    let models: Vec<ModelAgg> = agg
        .models
        .into_iter()
        .map(|mut m| {
            m.source_name = Some(name_by_id.get(&m.source_id).cloned().unwrap_or_else(|| m.source_id.clone()));
            m
        })
        .collect();

    // 配额（按源配置顺序；local-rate-limits 类过期很久（>7 天）视为历史快照不展示）
    let now = now_ms();
    let stale_ms: i64 = 7 * 86400000;
    let quota: Vec<QuotaInfo> = enabled
        .iter()
        .filter_map(|s| history.data().quota.get(&s.id).cloned())
        .filter(|q| !q.items.is_empty() || q.error.is_some())
        .map(|mut q| {
            if q.mode.as_deref() == Some("local-rate-limits") {
                q.items.retain(|it| {
                    !(it.resets_at.is_some() && it.resets_at.unwrap() < now - stale_ms)
                });
            }
            q
        })
        .filter(|q| !q.items.is_empty() || q.error.is_some())
        .collect();

    let g = config.get_general();
    Snapshot {
        range: range.to_string(),
        source_filter: source_id.to_string(),
        totals: agg.totals,
        models,
        trend: agg.trend,
        heatmap: agg.heatmap,
        active_days: agg.active_days,
        sources,
        quota,
        sources_meta: enabled
            .iter()
            .map(|s| SourcesMeta {
                id: s.id.clone(),
                name: s.name.clone(),
                type_: s.type_.clone(),
                enabled: s.enabled,
                display_order: s.display_order,
            })
            .collect(),
        general: SnapshotGeneral {
            show_cost: g.show_cost,
            show_quota: g.show_quota,
            show_charts: g.show_charts,
            show_model_list: g.show_model_list,
            opacity: g.opacity,
        },
        updated_at: history.data().last_updated,
        now,
    }
}

/// 冒烟模式辅助：不依赖 ConfigStore 的临时源列表构建快照
pub fn build_snapshot_with_sources(
    sources: &[SourceConfig],
    history: &HistoryStore,
    range: &str,
) -> Snapshot {
    // 构造一个内存 ConfigStore 变体：直接手动聚合
    let enabled: Vec<&SourceConfig> = sources.iter().filter(|s| s.enabled).collect();
    let ids: Vec<String> = enabled.iter().map(|s| s.id.clone()).collect();
    let agg = history.aggregate(&ids, range, &HashMap::new());
    let sources_agg: Vec<SourceAgg> = enabled
        .iter()
        .map(|s| {
            let a = history.aggregate(std::slice::from_ref(&s.id), range, &HashMap::new());
            SourceAgg {
                id: s.id.clone(),
                name: s.name.clone(),
                type_: s.type_.clone(),
                enabled: s.enabled,
                tokens: a.totals.tokens,
                cost: a.totals.cost,
                cost_partial: a.totals.cost_partial,
            }
        })
        .collect();
    Snapshot {
        range: range.to_string(),
        source_filter: "all".to_string(),
        totals: agg.totals,
        models: agg.models,
        trend: agg.trend,
        heatmap: agg.heatmap,
        active_days: agg.active_days,
        sources: sources_agg,
        quota: Vec::new(),
        sources_meta: enabled
            .iter()
            .map(|s| SourcesMeta {
                id: s.id.clone(),
                name: s.name.clone(),
                type_: s.type_.clone(),
                enabled: s.enabled,
                display_order: s.display_order,
            })
            .collect(),
        general: SnapshotGeneral {
            show_cost: true,
            show_quota: true,
            show_charts: true,
            show_model_list: true,
            opacity: 0.94,
        },
        updated_at: history.data().last_updated,
        now: now_ms(),
    }
}
