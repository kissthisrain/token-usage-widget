'use strict';
// 实时监听器：tail-f 式跟踪本地 JSONL 文件追加
// - fs.watch 毫秒级事件 → 读取增量 → 解析 → 聚合 → 推送
// - 30s 轮询兜底（文件轮转/事件丢失）
// - 偏移量持久化，重启不重复统计

const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');
const { parseFile, listJsonlFiles, scanParamsForSource } = require('./sources/local-logs');
const { importZcodeHistory } = require('./sources/sqlite-import');
const { daysAgoKey } = require('./util');

const POLL_MS = 30000;
// 自愈阈值：3 分钟无进展且未消费数据 > 1MB 时，重置偏移全量重扫
const STALL_MS = 3 * 60000;
const STALL_MIN_DELTA = 1024 * 1024;

/** 多行安全 JSON 记录提取：从 pos 开始找 '{'，扫描配对的 '}'（容忍记录内换行/转义） */
function extractJsonRecord(buf, from) {
  let i = from;
  while (i < buf.length && buf[i] !== 0x7b /* '{' */) i++;
  if (i >= buf.length) return null;
  let depth = 0, inStr = false, esc = false;
  for (let j = i; j < buf.length; j++) {
    const c = buf[j];
    if (inStr) {
      if (esc) esc = false;
      else if (c === 0x5c) esc = true;
      else if (c === 0x22) inStr = false;
      continue;
    }
    if (c === 0x22) inStr = true;
    else if (c === 0x7b) depth++;
    else if (c === 0x7d) {
      depth--;
      if (depth === 0) return { start: i, end: j + 1 };
    }
  }
  return null; // 未闭合（写入中）
}

class Watcher extends EventEmitter {
  constructor({ history, log }) {
    super();
    this.history = history;
    this.log = log || console;
    this._watchers = new Map(); // sourceId -> fs.FSWatcher
    this._pollTimer = null;
    this._scanning = new Set();
    this._lastAdvance = new Map(); // sourceId -> ts（自愈检测）
  }

  start() {
    this._pollTimer = setInterval(() => this.pollAll(), POLL_MS);
  }

  stop() {
    if (this._pollTimer) clearInterval(this._pollTimer);
    for (const w of this._watchers.values()) {
      try { w.close(); } catch { /* noop */ }
    }
    this._watchers.clear();
  }

  /** 重建监听（配置变更后调用） */
  restart(sources) {
    this.stop();
    this.start();
    for (const src of sources) this.watchSource(src);
  }

  watchSource(src) {
    if (src.type !== 'local-logs' || !src.enabled || !src.path) return;
    const dir = src.path;
    if (!fs.existsSync(dir)) return;
    const params = scanParamsForSource(src);

    try {
      const w = fs.watch(dir, { recursive: params.recursive }, (evt, filename) => {
        if (filename && String(filename).match(params.pattern)) {
          this.scanSource(src, params);
        } else if (evt === 'rename') {
          this.scanSource(src, params);
        }
      });
      this._watchers.set(src.id, w);
    } catch (e) {
      this.log.warn(`[watcher] ${src.name} 监听失败: ${e.message}`);
    }
    this._lastAdvance.set(src.id, Date.now());
    // 初始扫描（含增量）
    this.scanSource(src, params);
    // ZCode 数据库历史回填（rollout 只留当天，历史在 db.sqlite；仅回填今天之前，异步不阻塞）
    if (src.tool === 'zcode' && src.importSqlite !== false) {
      setTimeout(() => {
        importZcodeHistory({ source: src, history: this.history, log: this.log }).then((r) => {
          if (r.days > 0) this.emit('records', { sourceId: src.id, count: r.imported });
        });
      }, 300);
    }
  }

  pollAll() {
    // 兜底扫描 + 卡死自愈
    if (this.getSources) {
      for (const src of this.getSources()) {
        if (src.type !== 'local-logs' || !src.enabled || !src.path) continue;
        if (!fs.existsSync(src.path)) continue;
        const params = scanParamsForSource(src);
        this.scanSource(src, params);
        this.healIfStalled(src, params);
      }
    }
  }

  /** 自愈：长时间未消费且文件持续增长 → 重置偏移 + 清除近期数据 + 全量重扫 */
  healIfStalled(src, params) {
    const last = this._lastAdvance.get(src.id) || 0;
    if (Date.now() - last < STALL_MS) return;
    // 统计未消费量
    let totalSize = 0, consumed = 0;
    for (const f of listJsonlFiles(src.path, params.recursive, params.pattern)) {
      try { totalSize += fs.statSync(f).size; } catch { /* noop */ }
      consumed += this.history.getFileOffset(src.id, f) || 0;
    }
    if (totalSize - consumed < STALL_MIN_DELTA) {
      this._lastAdvance.set(src.id, Date.now()); // 数据没增长，不算卡死
      return;
    }
    this.log.warn(`[watcher] ${src.name} 疑似卡死（${Math.round((totalSize - consumed) / 1024)}KB 未消费），重置偏移全量重扫…`);
    for (const f of listJsonlFiles(src.path, params.recursive, params.pattern)) {
      this.history.setFileOffset(src.id, f, 0);
    }
    // 清除该源最近 35 天数据（< 今天由 sqlite 回填恢复，今天由全量重扫恢复）
    const cutoff = daysAgoKey(34);
    for (const k of Object.keys(this.history.days)) {
      if (k >= cutoff && this.history.days[k][src.id]) {
        delete this.history.days[k][src.id];
      }
    }
    this._lastAdvance.set(src.id, Date.now());
    this.scanSource(src, params);
    if (src.tool === 'zcode' && src.importSqlite !== false) {
      importZcodeHistory({ source: src, history: this.history, log: this.log });
    }
    this.emit('records', { sourceId: src.id, count: 0, healed: true });
  }

  /** 绑定源列表提供者（避免循环依赖） */
  setSourceProvider(fn) {
    this.getSources = fn;
  }

  scanSource(src, params) {
    if (this._scanning.has(src.id)) return;
    this._scanning.add(src.id);
    try {
      const files = listJsonlFiles(src.path, params.recursive, params.pattern);
      for (const f of files) this.readDelta(src, params, f);
    } finally {
      this._scanning.delete(src.id);
    }
  }

  readDelta(src, params, file) {
    let size;
    try { size = fs.statSync(file).size; } catch { return; }
    if (size === 0) return;
    const off = this.history.getFileOffset(src.id, file);
    let start = off;
    if (start > size) start = 0; // 文件被轮转/替换
    if (start === size) return;

    let fd = null;
    let buf;
    try {
      fd = fs.openSync(file, 'r');
      buf = Buffer.alloc(size - start);
      fs.readSync(fd, buf, 0, buf.length, start);
    } catch (e) {
      return;
    } finally {
      if (fd != null) fs.closeSync(fd);
    }

    // codex 文件需要整体有序解析 → 直接全量重读该文件
    if (params.format === 'codex') {
      const { records, rateLimits } = parseFile(file, 'codex');
      if (records.length) {
        this.history.addRecords(src.id, records);
        this.emit('records', { sourceId: src.id, count: records.length });
      }
      if (rateLimits.length) {
        this.history.setQuota(src.id, {
          sourceId: src.id,
          items: [{ label: rateLimits[rateLimits.length - 1].label, ...rateLimits[rateLimits.length - 1] }],
          mode: 'local-rate-limits',
          local: true,
        });
        this.emit('quota', { sourceId: src.id });
      }
      this.history.setFileOffset(src.id, file, size);
      this._lastAdvance.set(src.id, Date.now());
      return;
    }

    // 多行安全解析：花括号配对提取记录，损坏片段跳过，尾部未写完等待下次
    const records = [];
    let pos = 0;
    while (pos < buf.length) {
      const rec = extractJsonRecord(buf, pos);
      if (!rec) break; // 未找到完整记录（写入中/损坏）→ 保留偏移等待
      let r;
      try {
        r = JSON.parse(buf.toString('utf8', rec.start, rec.end));
      } catch {
        pos = rec.start + 1; // 损坏片段 → 跳过继续
        continue;
      }
      const p = parseRecord(r);
      if (p) records.push(p);
      pos = rec.end;
    }
    if (records.length) {
      this.history.addRecords(src.id, records);
      this.emit('records', { sourceId: src.id, count: records.length });
    }
    // 已完整消费的字节数（防止偏移越过文件末尾）
    const endOffset = Math.min(start + pos, size);
    if (endOffset > start) {
      this.history.setFileOffset(src.id, file, endOffset);
      this._lastAdvance.set(src.id, Date.now());
    }
  }
}

function parseRecord(r) {
  // 与 local-logs 的 zcode 解析保持一致（避免循环引用）
  const u = r && r.response && r.response.usage;
  const total = u && (u.totalTokens ?? u.total_tokens);
  if (total == null || !isFinite(total) || total <= 0) return null;
  return {
    ts: Date.parse(r.completedAt || r.timestamp) || Date.now(),
    model: (r.model && (r.model.modelId || r.model.id)) || r.model || null,
    input: u.inputTokens ?? u.input_tokens ?? null,
    output: u.outputTokens ?? u.output_tokens ?? null,
    total,
  };
}

module.exports = { Watcher, extractJsonRecord };
