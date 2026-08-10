'use strict';
// 配置读写：userData/config.json

const fs = require('fs');
const path = require('path');

const DEFAULT_CONFIG = {
  version: 1,
  general: {
    refreshIntervalMin: 10,     // API 类数据源拉取间隔（分钟）
    hotkey: 'CommandOrControl+Alt+U',
    autoLaunch: true,
    showCost: true,             // 显示费用
    showQuota: true,            // 显示配额区块
    showCharts: true,           // 显示趋势图 + 热力图
    showModelList: true,        // 显示模型明细
    opacity: 0.94,
    alwaysOnTop: false,         // 钉在前排（默认不置顶，点击左上角图钉切换）
  },
  sources: [],
};

class ConfigStore {
  constructor(dir) {
    this.dir = dir;
    this.file = path.join(dir, 'config.json');
    this.data = null;
  }

  load() {
    let loaded = null;
    try {
      loaded = JSON.parse(fs.readFileSync(this.file, 'utf8'));
    } catch {
      loaded = null;
    }
    const base = JSON.parse(JSON.stringify(DEFAULT_CONFIG));
    this.data = {
      ...base,
      ...(loaded || {}),
      general: { ...base.general, ...((loaded && loaded.general) || {}) },
    };
    if (!Array.isArray(this.data.sources)) this.data.sources = [];
    if (!this.data.sources.length) this.save();
    return this.data;
  }

  save() {
    fs.mkdirSync(this.dir, { recursive: true });
    fs.writeFileSync(this.file, JSON.stringify(this.data, null, 2));
  }

  get() { return this.data; }
  getGeneral() { return this.data.general; }
  getSources(enabledOnly = false) {
    return enabledOnly ? this.data.sources.filter((s) => s.enabled) : this.data.sources;
  }
  getSource(id) { return this.data.sources.find((s) => s.id === id) || null; }

  addSource(cfg) {
    const s = {
      id: this.genId('src'),
      name: '数据源',
      enabled: true,
      displayOrder: this.data.sources.length,
      modelWhitelist: [],
      ...cfg,
    };
    this.data.sources.push(s);
    this.save();
    return s;
  }

  updateSource(cfg) {
    const i = this.data.sources.findIndex((s) => s.id === cfg.id);
    if (i < 0) return null;
    this.data.sources[i] = { ...this.data.sources[i], ...cfg };
    this.save();
    return this.data.sources[i];
  }

  removeSource(id) {
    this.data.sources = this.data.sources.filter((s) => s.id !== id);
    this.save();
  }

  genId(prefix) {
    return `${prefix}_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`;
  }
}

module.exports = { ConfigStore, DEFAULT_CONFIG };
