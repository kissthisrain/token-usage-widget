'use strict';
// 快照构建：主进程 → 渲染进程的统一数据视图

const { computeQuota } = require('./sources/opencode-go');

function buildSnapshot({ config, history, range = 'month', sourceId = 'all' }) {
  const enabled = config.getSources(true);
  const ids = enabled.map((s) => s.id);

  let sourceIds = ids;
  if (sourceId && sourceId !== 'all' && ids.includes(sourceId)) {
    sourceIds = [sourceId];
  }

  // 模型白名单
  const whitelist = new Map();
  for (const s of enabled) {
    if (Array.isArray(s.modelWhitelist) && s.modelWhitelist.length) {
      whitelist.set(s.id, new Set(s.modelWhitelist));
    }
  }

  const agg = history.aggregate({ sourceIds, range, whitelist });

  // 每个启用的源：独立聚合 + 配额
  const sources = enabled.map((s) => {
    const a = history.aggregate({ sourceIds: [s.id], range });
    return {
      id: s.id,
      name: s.name,
      type: s.type,
      enabled: s.enabled,
      tokens: a.totals.tokens,
      cost: a.totals.cost,
      costPartial: a.totals.costPartial,
    };
  });

  // opencode-go 配额：
  // - 配置了官方查询（工作区 ID + Cookie）→ 用调度器缓存的官方数据（权威），
  //   缓存缺失（首屏/调度器未跑）时用本地估算兜底
  // - 未配置 → 每次快照本地动态计算（代理记录估算）
  for (const s of enabled) {
    if (s.type !== 'opencode-go') continue;
    if (s.workspaceId && s.authCookie) {
      const q = history.quota[s.id];
      if (!q || (!(q.items && q.items.length) && !q.error)) {
        history.setQuota(s.id, computeQuota({ history, source: s }));
      }
    } else {
      history.setQuota(s.id, computeQuota({ history, source: s }));
    }
  }

  const nameById = Object.fromEntries(enabled.map((s) => [s.id, s.name]));
  const models = agg.models.map((m) => ({
    ...m,
    sourceName: nameById[m.sourceId] || m.sourceId,
  }));

  // 配额（按源配置顺序；本地 rate-limits 类若已过期很久（>7 天）视为历史快照，不展示）
  const now = Date.now();
  const STALE_MS = 7 * 86400000;
  const quota = enabled
    .map((s) => history.quota[s.id] || null)
    .filter((q) => q && ((q.items && q.items.length) || q.error))
    .map((q) => {
      if (!q.items) return q;
      return {
        ...q,
        items: q.items.filter((it) => !(q.mode === 'local-rate-limits' && it.resetsAt && it.resetsAt < now - STALE_MS)),
      };
    })
    .filter((q) => !q.items || q.items.length || q.error);

  return {
    range,
    sourceFilter: sourceId || 'all',
    totals: agg.totals,
    models,
    trend: agg.trend,
    heatmap: agg.heatmap,
    activeDays: agg.activeDays,
    sources,
    quota,
    sourcesMeta: enabled.map((s) => ({
      id: s.id,
      name: s.name,
      type: s.type,
      enabled: s.enabled,
      displayOrder: s.displayOrder,
    })),
    general: (() => {
      const g = config.getGeneral ? config.getGeneral() : {};
      return {
        showCost: g.showCost !== false,
        showQuota: g.showQuota !== false,
        showCharts: g.showCharts !== false,
        showModelList: g.showModelList !== false,
        opacity: Number(g.opacity) || 0.94,
      };
    })(),
    updatedAt: history.lastUpdated,
    now: Date.now(),
  };
}

module.exports = { buildSnapshot };
