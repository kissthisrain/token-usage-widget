'use strict';
// 历史数据存储：userData/history.json
// 结构：
// {
//   version: 1,
//   days: { 'YYYY-MM-DD': { [sourceId]: { [model]: {tokens,input,output,cost,calls} } } },
//   recent: [{ ts, sourceId, model, input, output, total, cost }],   // 近 30 天原始记录（用于 5h 窗口等细粒度计算），上限 20000 条
//   quota: { [sourceId]: { items: [...], mode, error, at } },
//   fileOffsets: { [sourceId]: { [filePath]: bytes } },              // 文件监听偏移量
//   activeDays 由 days 推导
// }

const fs = require('fs');
const path = require('path');
const { dateKey, todayKey, daysAgoKey, parseTs } = require('./util');

const RECENT_MAX = 20000;
const RECENT_KEEP_DAYS = 35;

class HistoryStore {
  constructor(dir) {
    this.file = path.join(dir, 'history.json');
    this.days = {};
    this.recent = [];
    this.quota = {};
    this.fileOffsets = {};
    this.lastUpdated = 0;
    this._saveTimer = null;
  }

  load() {
    try {
      const d = JSON.parse(fs.readFileSync(this.file, 'utf8'));
      this.days = d.days || {};
      this.recent = d.recent || [];
      this.quota = d.quota || {};
      this.fileOffsets = d.fileOffsets || {};
      this.lastUpdated = d.lastUpdated || 0;
    } catch {
      this.days = {};
      this.recent = [];
      this.quota = {};
      this.fileOffsets = {};
    }
    return this;
  }

  scheduleSave() {
    if (this._saveTimer) return;
    this._saveTimer = setTimeout(() => {
      this._saveTimer = null;
      this.save();
    }, 800);
  }

  save() {
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true });
      const payload = {
        version: 1,
        days: this.days,
        recent: this.recent,
        quota: this.quota,
        fileOffsets: this.fileOffsets,
        lastUpdated: this.lastUpdated,
      };
      fs.writeFileSync(this.file, JSON.stringify(payload));
    } catch (e) {
      console.error('[history] save 失败:', e.message);
    }
  }

  touch() {
    this.lastUpdated = Date.now();
    this.scheduleSave();
  }

  /** 记录归一化：{ts, sourceId, model, provider?, input, output, total, cacheRead?, cost?}
   *  opts.recent: 是否写入原始记录缓冲（默认 true；API 全量源传 false） */
  addRecords(sourceId, records, opts = {}) {
    const useRecent = opts.recent !== false;
    let changed = false;
    for (const r of records) {
      if (r == null || r.total == null || !isFinite(r.total) || r.total <= 0) continue;
      const ts = parseTs(r.ts) || Date.now();
      const key = dateKey(ts);
      const model = r.model || '(unknown)';
      const m = (this.days[key] ??= {});
      const s = (m[sourceId] ??= {});
      const a = (s[model] ??= { tokens: 0, input: 0, output: 0, cost: 0, calls: 0 });
      a.tokens += r.total;
      a.input += r.input || 0;
      a.output += r.output || 0;
      if (r.cost != null && isFinite(r.cost)) a.cost += r.cost;
      a.calls += 1;
      // 原始记录（细粒度窗口用）
      if (useRecent && ts >= Date.now() - RECENT_KEEP_DAYS * 86400000) {
        this.recent.push({
          ts, sourceId, model,
          input: r.input || 0, output: r.output || 0,
          total: r.total,
          cost: r.cost != null && isFinite(r.cost) ? r.cost : null,
        });
      }
      changed = true;
    }
    if (changed) {
      // 裁剪 recent
      const cutoff = Date.now() - RECENT_KEEP_DAYS * 86400000;
      this.recent = this.recent.filter((r) => r.ts >= cutoff);
      if (this.recent.length > RECENT_MAX) this.recent = this.recent.slice(-RECENT_MAX);
      this.touch();
    }
  }

  /** API 全量源：用一次拉取结果整体替换某天某源的数据（幂等，防重复累计）
   *  models: { [model]: {tokens, input, output, cost, calls} } */
  setSourceDay(sourceId, dayKey, models) {
    const m = (this.days[dayKey] ??= {});
    if (models && Object.keys(models).length) {
      m[sourceId] = models;
    } else {
      delete m[sourceId];
    }
    this.touch();
  }

  setQuota(sourceId, quotaInfo) {
    if (!quotaInfo) return;
    this.quota[sourceId] = { ...quotaInfo, at: Date.now() };
    this.touch();
  }

  clear() {
    this.days = {};
    this.recent = [];
    this.quota = {};
    this.fileOffsets = {};
    this.lastUpdated = 0;
    this.save();
  }

  getFileOffset(sourceId, file) {
    return (this.fileOffsets[sourceId] && this.fileOffsets[sourceId][file]) || 0;
  }

  setFileOffset(sourceId, file, bytes) {
    (this.fileOffsets[sourceId] ??= {})[file] = bytes;
  }

  /** 范围内聚合。range: 'day'|'month'|'total'；sourceIds: 参与聚合的源；
   *  whitelist: Map<sourceId, Set<model>> 模型白名单（不在名单内的模型不计入） */
  aggregate({ sourceIds, range, whitelist }) {
    const keys = Object.keys(this.days).sort();
    const today = todayKey();
    const monthStart = daysAgoKey(29);
    const inRange = (k) => {
      if (range === 'day') return k >= today;
      if (range === 'month') return k >= monthStart;
      return true;
    };
    const ids = new Set(sourceIds);
    const allowModel = (sid, model) => {
      const wl = whitelist && whitelist.get(sid);
      return !wl || wl.has(model);
    };

    let tokens = 0, cost = 0, costPartial = false;
    const modelMap = new Map();   // `${sourceId}|${model}` → {model, sourceId, sourceName, tokens, cost, costPartial, calls}
    const trendMap = new Map();   // key → {date, tokens, cost, costPartial}
    const activeSet = new Set();

    for (const k of keys) {
      if (!inRange(k)) continue;
      const day = this.days[k];
      let dayTokens = 0;
      for (const sid of Object.keys(day)) {
        if (!ids.has(sid)) continue;
        const src = day[sid];
        for (const model of Object.keys(src)) {
          if (!allowModel(sid, model)) continue;
          const a = src[model];
          dayTokens += a.tokens;
          if (a.tokens > 0) activeSet.add(k);
          const mk = `${sid}|${model}`;
          let mm = modelMap.get(mk);
          if (!mm) {
            mm = { model, sourceId: sid, tokens: 0, cost: 0, costPartial: false, calls: 0 };
            modelMap.set(mk, mm);
          }
          mm.tokens += a.tokens;
          mm.cost += a.cost;
          mm.calls += a.calls;
        }
      }
      if (dayTokens > 0) {
        const t = trendMap.get(k) || { date: k, tokens: 0, cost: 0, costPartial: false };
        t.tokens += dayTokens;
        trendMap.set(k, t);
      }
    }

    // 热力图：最近 26 周每天（不随 range 过滤，展示全量活动，与参考图一致）
    const heatStart = daysAgoKey(26 * 7 - 1);
    const heatmapMap = new Map();
    for (const k of keys) {
      if (k < heatStart) continue;
      let dayTokens = 0;
      const day = this.days[k];
      if (!day) continue;
      for (const sid of Object.keys(day)) {
        if (!ids.has(sid)) continue;
        const src = day[sid];
        for (const model of Object.keys(src)) {
          if (!allowModel(sid, model)) continue;
          dayTokens += src[model].tokens;
        }
      }
      if (dayTokens > 0) heatmapMap.set(k, dayTokens);
    }
    // 活跃天数：全量（与热力图口径一致）
    const activeDaysAll = new Set();
    for (const k of keys) {
      const day = this.days[k];
      if (!day) continue;
      for (const sid of Object.keys(day)) {
        if (!ids.has(sid)) continue;
        for (const model of Object.keys(day[sid])) {
          if (allowModel(sid, model) && day[sid][model].tokens > 0) {
            activeDaysAll.add(k);
            break;
          }
        }
      }
    }
    const activeDays = activeDaysAll.size;

    // 成本汇总（含 partial 标记）
    for (const mm of modelMap.values()) {
      tokens += mm.tokens;
      cost += mm.cost;
    }
    // recent 里 cost 为 null 的记录 → costPartial
    const knownCostModelKeys = new Set();
    for (const r of this.recent) {
      if (r.cost == null && ids.has(r.sourceId)) costPartial = true;
    }

    const models = [...modelMap.values()]
      .map((mm) => ({ ...mm }))
      .sort((a, b) => b.tokens - a.tokens);
    const maxTokens = models.length ? models[0].tokens : 0;
    for (const mm of models) mm.pct = maxTokens ? Math.round((mm.tokens / maxTokens) * 100) : 0;

    const trend = [...trendMap.values()].sort((a, b) => (a.date < b.date ? -1 : 1));

    // 热力图数据：最近 26 周每天
    const heatmap = [];
    for (let i = 0; i < 26 * 7; i++) {
      const k = dateKey(Date.now() - i * 86400000);
      if (k < heatStart) break;
      heatmap.push({ date: k, tokens: heatmapMap.get(k) || 0 });
    }
    heatmap.reverse();

    return { totals: { tokens, cost, costPartial }, models, trend, heatmap, activeDays };
  }

  /** opencode-go 类：基于历史算窗口内美元消耗 */
  spentInWindow({ sourceId, ms }) {
    const cutoff = Date.now() - ms;
    let spent = 0, partial = false, firstTs = null;
    for (const r of this.recent) {
      if (r.sourceId !== sourceId || r.ts < cutoff) continue;
      if (r.cost == null) { partial = true; continue; }
      spent += r.cost;
      if (firstTs == null || r.ts < firstTs) firstTs = r.ts;
    }
    // recent 保留 35 天，覆盖 30 天月窗口
    if (ms > RECENT_KEEP_DAYS * 86400000) partial = true;
    return { spent, partial, firstTs };
  }

  spentInCalendarMonth({ sourceId }) {
    const monthKey = todayKey().slice(0, 7); // YYYY-MM
    let spent = 0, partial = false;
    for (const k of Object.keys(this.days)) {
      if (!k.startsWith(monthKey)) continue;
      const src = this.days[k][sourceId];
      if (!src) continue;
      for (const model of Object.keys(src)) spent += src[model].cost || 0;
    }
    return { spent, partial };
  }
}

module.exports = { HistoryStore };
