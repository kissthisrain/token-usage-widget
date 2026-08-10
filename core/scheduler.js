'use strict';
// 定时调度器：周期性拉取 API 类数据源（openai / compat）
// - openai: 拉取按天用量 → 幂等替换历史（不重复累计）
// - compat: 余额探测 → 写配额缓存

const { EventEmitter } = require('events');
const { fetchUsage } = require('./sources/openai');
const { probe } = require('./sources/compat');
const { probeQuota } = require('./sources/opencode-go');
const { dateKey } = require('./util');

class Scheduler extends EventEmitter {
  constructor({ config, history, log }) {
    super();
    this.config = config;
    this.history = history;
    this.log = log || console;
    this._timer = null;
    this._running = false;
  }

  start() {
    const min = Math.max(1, Number(this.config.getGeneral().refreshIntervalMin) || 10);
    this.stop();
    this._timer = setInterval(() => this.tick(), min * 60000);
    // 启动即拉取一次
    setTimeout(() => this.tick(), 1500);
  }

  stop() {
    if (this._timer) clearInterval(this._timer);
    this._timer = null;
  }

  async tick() {
    if (this._running) return;
    this._running = true;
    try {
      const sources = this.config.getSources(true);
      for (const src of sources) {
        try {
          if (src.type === 'openai') {
            const records = await fetchUsage(src, { days: 90 });
            // 按天分桶 → 幂等替换
            const byDay = new Map();
            for (const r of records) {
              const k = dateKey(r.ts);
              if (!byDay.has(k)) byDay.set(k, {});
              const model = r.model || '(unknown)';
              const day = byDay.get(k);
              const a = (day[model] ??= { tokens: 0, input: 0, output: 0, cost: 0, calls: 0 });
              a.tokens += r.total;
              a.input += r.input || 0;
              a.output += r.output || 0;
              if (r.cost != null) a.cost += r.cost;
              a.calls += 1;
            }
            for (const [k, models] of byDay) {
              this.history.setSourceDay(src.id, k, models);
            }
            this.history.setQuota(src.id, { sourceId: src.id, mode: 'openai-usage', items: [], ok: true, at: Date.now() });
            this.log.info(`[scheduler] ${src.name}: 用量更新 ${records.length} 条`);
            this.emit('usage', { sourceId: src.id });
          } else if (src.type === 'compat') {
            const r = await probe(src);
            if (r.ok) {
              this.history.setQuota(src.id, { sourceId: src.id, mode: r.mode, items: r.items, ok: true, at: Date.now() });
              this.log.info(`[scheduler] ${src.name}: 余额探测成功 (${r.mode})`);
            } else {
              this.history.setQuota(src.id, { sourceId: src.id, mode: null, items: [], ok: false, error: r.error, at: Date.now() });
              this.log.warn(`[scheduler] ${src.name}: ${r.error}`);
            }
            this.emit('quota', { sourceId: src.id });
          } else if (src.type === 'opencode-go' && src.workspaceId && src.authCookie) {
            // 官方用量查询（配置了工作区 ID + Cookie 时；未配置走 snapshot 本地估算）
            const r = await probeQuota(src);
            const prev = this.history.quota[src.id];
            if (r.ok) {
              this.history.setQuota(src.id, { sourceId: src.id, mode: 'opencode-go-official', items: r.items, ok: true, at: Date.now() });
              this.log.info(`[scheduler] ${src.name}: 官方用量更新 (${r.items.map((i) => `${i.key} ${i.usedPct.toFixed(1)}%`).join('/')})`);
            } else {
              this.history.setQuota(src.id, {
                sourceId: src.id,
                mode: 'opencode-go-official',
                items: (prev && prev.items) || [], // 保留上次成功数据，仅附加错误
                ok: false,
                error: r.error,
                at: Date.now(),
              });
              this.log.warn(`[scheduler] ${src.name}: ${r.error}`);
            }
            this.emit('quota', { sourceId: src.id });
          }
        } catch (e) {
          this.log.warn(`[scheduler] ${src.name}: ${e.message}`);
          if (src.type === 'openai') {
            this.history.setQuota(src.id, { sourceId: src.id, mode: 'openai', items: [], ok: false, error: e.message, at: Date.now() });
            this.emit('quota', { sourceId: src.id });
          }
        }
      }
    } finally {
      this._running = false;
    }
  }

  /** 立即刷新单个源（设置界面「立即刷新」用） */
  async refreshSource(src) {
    await this.tick();
  }
}

module.exports = { Scheduler };
