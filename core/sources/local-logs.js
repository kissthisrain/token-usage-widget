'use strict';
// 本地 JSONL 记录解析：支持 ZCode（camelCase）与 Codex（snake_case + 事件结构）格式
// 归一化记录: {ts, model, provider, input, output, total, cacheRead, cost?}

const fs = require('fs');
const path = require('path');
const { parseTs } = require('../util');

// ---------- ZCode 格式（model-io-*.jsonl） ----------
// 顶层: {completedAt, model:{modelId, providerId}, response:{usage:{inputTokens, outputTokens, totalTokens, cacheReadTokens, reasoningTokens}}}

function parseZcodeRecord(r) {
  if (typeof r !== 'object' || r == null) return null;
  const u = r.response && r.response.usage;
  const total = u && (u.totalTokens ?? u.total_tokens);
  if (total == null || !isFinite(total) || total <= 0) return null;
  return {
    ts: parseTs(r.completedAt) || parseTs(r.timestamp) || parseTs(r.startedAt) || null,
    model: (r.model && (r.model.modelId || r.model.id)) || r.model || null,
    provider: (r.model && r.model.providerId) || null,
    input: u.inputTokens ?? u.input_tokens ?? null,
    output: u.outputTokens ?? u.output_tokens ?? null,
    total,
    cacheRead: u.cacheReadTokens ?? u.cached_input_tokens ?? null,
  };
}

// ---------- Codex 格式（rollout-*.jsonl，需按时间顺序解析） ----------
// turn_context: {payload:{model}} → 记录当前模型
// event_msg: {payload:{type:'token_count', info:{last_token_usage:{input_tokens,...}, total_token_usage}, rate_limits}}
// 注意 token_count 事件可能重复（相同增量），用签名去重

function pickModelFromRecord(r) {
  const s = JSON.stringify(r);
  const m = s.match(/"model"\s*:\s*"([^"]+)"/);
  return m ? m[1] : null;
}

function normalizeRateLimits(rl, ts) {
  if (!rl || !rl.limit_id) return null;
  const p = rl.primary || {};
  const credits = rl.credits
    ? {
        hasCredits: !!rl.credits.has_credits,
        unlimited: !!rl.credits.unlimited,
        balance: rl.credits.balance != null ? rl.credits.balance : null,
      }
    : null;
  // 空占位（primary/credits 全空）→ 无数据
  const hasReal =
    p.used_percent != null ||
    p.window_minutes != null ||
    p.resets_at != null ||
    (credits && (credits.balance != null || credits.hasCredits || credits.unlimited));
  if (!hasReal) return null;
  return {
    limitId: rl.limit_id,
    label: rl.limit_name || rl.limit_id,
    usedPct: p.used_percent != null ? p.used_percent : null,
    windowMinutes: p.window_minutes != null ? p.window_minutes : null,
    resetsAt: p.resets_at != null ? p.resets_at * 1000 : null,
    credits,
    planType: rl.plan_type || null,
    at: ts,
  };
}

function parseCodexFile(filePath) {
  const records = [];
  const rateLimits = [];
  let curModel = null;
  let lastSig = null;

  let fd, text;
  try {
    fd = fs.openSync(filePath, 'r');
    const size = fs.fstatSync(fd).size;
    if (size === 0) return { records, rateLimits };
    const buf = Buffer.alloc(size);
    fs.readSync(fd, buf, 0, size, 0);
    text = buf.toString('utf8');
  } catch (e) {
    return { records, rateLimits };
  } finally {
    if (fd != null) fs.closeSync(fd);
  }

  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    let r;
    try { r = JSON.parse(line); } catch { continue; }
    if (typeof r !== 'object' || r == null) continue;

    if (r.type === 'turn_context' && r.payload && r.payload.model) {
      curModel = r.payload.model;
      continue;
    }
    if (r.type !== 'event_msg' || !r.payload || r.payload.type !== 'token_count') continue;

    const ts = parseTs(r.timestamp);
    const info = r.payload.info || {};
    const u = info.last_token_usage || info.total_token_usage;
    if (u) {
      const total = u.total_tokens;
      if (total != null && isFinite(total) && total > 0) {
        const sig = `${ts}|${curModel}|${u.input_tokens}|${u.output_tokens}|${total}`;
        if (sig !== lastSig) {
          lastSig = sig;
          records.push({
            ts: ts || Date.now(),
            model: curModel || pickModelFromRecord(r) || null,
            provider: 'codex',
            input: u.input_tokens ?? null,
            output: u.output_tokens ?? null,
            total,
            cacheRead: u.cached_input_tokens ?? null,
          });
        }
      }
    }
    const rl = normalizeRateLimits(r.payload.rate_limits, ts || Date.now());
    if (rl) rateLimits.push(rl);
  }
  return { records, rateLimits };
}

// ---------- 通用入口 ----------
function parseFile(filePath, format) {
  if (format === 'codex') return parseCodexFile(filePath);
  // zcode / 自定义：逐行 zcode 解析（容错，同时尝试 codex 字段名）
  const records = [];
  let text;
  try { text = fs.readFileSync(filePath, 'utf8'); } catch { return { records }; }
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    let r;
    try { r = JSON.parse(line); } catch { continue; }
    const rec = parseZcodeRecord(r);
    if (rec) records.push(rec);
  }
  return { records };
}

/** 目录扫描：非递归（zcode rollout）或递归（codex sessions/自定义） */
function listJsonlFiles(dir, recursive, pattern) {
  const out = [];
  const re = pattern instanceof RegExp ? pattern : new RegExp(pattern);
  const walk = (d) => {
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const ent of entries) {
      const p = path.join(d, ent.name);
      if (ent.isDirectory()) {
        if (recursive) walk(p);
      } else if (re.test(ent.name)) {
        out.push(p);
      }
    }
  };
  walk(dir);
  out.sort();
  return out;
}

/** 源配置文件 → 扫描参数 */
function scanParamsForSource(src) {
  const tool = src.tool || 'zcode';
  if (tool === 'codex') {
    return { recursive: true, pattern: /^rollout-.*\.jsonl$/i, format: 'codex' };
  }
  if (tool === 'zcode') {
    return { recursive: false, pattern: /^model-io-.*\.jsonl$/i, format: 'zcode' };
  }
  // 自定义
  return { recursive: !!src.recursive, pattern: src.filePattern || /\.jsonl$/i, format: src.format || 'zcode' };
}

module.exports = { parseZcodeRecord, parseCodexFile, parseFile, listJsonlFiles, scanParamsForSource, normalizeRateLimits };
