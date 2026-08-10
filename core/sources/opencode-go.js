'use strict';
// OpenCode Go 套餐适配器
// - 无公开用量 JSON API（/v1/usage、/v1/balance 均 404）→ 两种方案：
//   1) 本地代理（main/proxy.js）记录每次请求的 token 与费用（估算，仅统计本机）
//   2) 官方用量查询（probeQuota）：用浏览器登录态 Cookie + 工作区 ID 抓取
//      https://opencode.ai/workspace/{id}/go 控制台页面，解析内嵌的
//      rollingUsage(5h) / weeklyUsage / monthlyUsage 的 usagePercent + resetInSec
//      （页面为 SSR 渲染，权威数据，第三方工具 @slkiser/opencode-quota 同方案）
// - 官方限额（美元）：5 小时 $12 / 每周 $30 / 每月 $60
// - 费用 = 输入 token/1M × 输入单价 + 输出 token/1M × 输出单价（官方价目表）
// - 剩余% = (限额 - 窗口内消耗) / 限额

const { getJson, errMsg } = require('../http');

// 官方价目表（USD / 1M tokens，2026-08 抓取自 opencode.ai/zh/go）
// 模型 id 可能带 "opencode-go/" 前缀，均归一化处理
const DEFAULT_PRICE_MAP = {
  'grok-4.5': { input: 2.0, output: 6.0 },
  'gpt-5.6-luna': { input: 0.2, output: 1.2 },
  'glm-5.2': { input: 1.4, output: 4.4 },
  'glm-5.1': { input: 1.4, output: 4.4 },
  'glm-5': { input: 1.4, output: 4.4 },
  'kimi-k3': { input: 3.0, output: 15.0 },
  'kimi-k2.7-code': { input: 0.95, output: 4.0 },
  'kimi-k2.6': { input: 0.95, output: 4.0 },
  'kimi-k2.5': { input: 0.95, output: 4.0 },
  'mimo-v2.5': { input: 0.14, output: 0.28 },
  'mimo-v2.5-pro': { input: 0.435, output: 0.87 },
  'mimo-v2-pro': { input: 0.435, output: 0.87 },
  'mimo-v2-omni': { input: 0.14, output: 0.28 },
  'minimax-m3': { input: 0.3, output: 1.2 },
  'minimax-m2.7': { input: 0.3, output: 1.2 },
  'minimax-m2.5': { input: 0.3, output: 1.2 },
  'qwen3.8-max': { input: 2.0, output: 6.0 },
  'qwen3.7-max': { input: 2.5, output: 7.5 },
  'qwen3.7-plus': { input: 0.4, output: 1.6 },
  'qwen3.6-plus': { input: 0.5, output: 3.0 },
  'qwen3.5-plus': { input: 0.5, output: 3.0 },
  'deepseek-v4-pro': { input: 0.435, output: 0.87 },
  'deepseek-v4-flash': { input: 0.14, output: 0.28 },
  'hy3': { input: 0.14, output: 0.58 },
  'hy3-preview': { input: 0.14, output: 0.58 },
};

// 官方限额
const LIMITS = [
  { key: '5h', label: '5 小时', limitUsd: 12, windowMs: 5 * 3600 * 1000 },
  { key: 'week', label: '每周', limitUsd: 30, windowMs: 7 * 86400 * 1000 },
  { key: 'month', label: '每月', limitUsd: 60, windowMs: 0 }, // 日历月
];

function modelKey(model) {
  return String(model || '').replace(/^opencode-go\//, '');
}

/** 计算一次请求费用（美元），未知模型返回 null */
function costFor(model, input, output, priceMap) {
  const p = (priceMap || {})[modelKey(model)] || DEFAULT_PRICE_MAP[modelKey(model)];
  if (!p) return null;
  return ((input || 0) / 1e6) * p.input + ((output || 0) / 1e6) * p.output;
}

function monthEndTs(now = Date.now()) {
  const d = new Date(now);
  return new Date(d.getFullYear(), d.getMonth() + 1, 1).getTime();
}

function round4(x) {
  return Math.round(x * 10000) / 10000;
}

function clampPct(p) {
  if (!isFinite(p)) return null;
  return Math.max(0, Math.min(100, p));
}

/** 解析控制台重置时间的自然语言文本 → 秒（"6 days 2 hours" / "3h 42m" / "6 天 2 小时"），失败返回 null */
function parseHumanDuration(text) {
  const t = String(text || '');
  if (!t) return null;
  if (!/\d/.test(t)) return /reset|now|立即|马上/i.test(t) ? 0 : null;
  const TOKENS = [
    [/(?:day|天)/gi, 86400],
    [/(?:hour|小时)/g, 3600],
    [/(?:min|分钟)/g, 60],
    [/(?:sec|秒)/g, 1],
    [/\bh\b/g, 3600],
    [/\bm\b/g, 60],
    [/\bs\b/g, 1],
  ];
  let total = null;
  for (const [re, sec] of TOKENS) {
    for (const m of t.matchAll(new RegExp(`(\\d+)\\s*${re.source}`, re.flags))) {
      total = (total || 0) + Number(m[1]) * sec;
    }
  }
  return isFinite(total) ? total : null;
}

// 控制台页面窗口名 → 限额 key
const SSR_NAMES = [
  ['rollingUsage', '5h'],
  ['weeklyUsage', 'week'],
  ['monthlyUsage', 'month'],
];

/**
 * 从控制台页面 HTML 提取三个窗口的用量百分比与重置时间。
 * 两种内嵌格式：
 *  1) SSR 水合 JSON：...rollingUsage:$R[n]={usagePercent:23.5,resetInSec:13320}...weeklyUsage:...monthlyUsage:...
 *  2) data-slot HTML：<div data-slot="usage-item"><div data-slot="usage-label">Rolling Usage</div>
 *     <div data-slot="usage-value">23%</div><div data-slot="reset-time">3 hours 42 minutes</div></div>
 */
function extractUsage(html) {
  const text = String(html || '');
  const out = {};
  for (const [name, key] of SSR_NAMES) {
    const idx = text.indexOf(name);
    if (idx < 0) continue;
    const seg = text.slice(idx, idx + 400);
    const pct = seg.match(/usagePercent["']?\s*[:=]\s*["']?([\d.]+)/);
    const sec = seg.match(/resetInSec["']?\s*[:=]\s*["']?(\d+)/);
    if (pct) {
      const p = clampPct(Number(pct[1]));
      if (p != null) out[key] = { usedPct: p, resetsAt: sec ? Date.now() + Number(sec[1]) * 1000 : null };
    }
  }
  // data-slot 兜底（水合数据缺失时）
  if (Object.keys(out).length < SSR_NAMES.length) {
    const slots = text
      .split('data-slot="usage-item"')
      .slice(1)
      .map((b) => {
        const rt = b.match(/data-slot="reset-time">([^<]*)</);
        const rn = b.match(/data-slot="reset-now"/);
        return {
          label: (b.match(/data-slot="usage-label">([^<]*)</) || [])[1] || '',
          value: (b.match(/data-slot="usage-value">([^<]*)</) || [])[1] || '',
          reset: rt ? rt[1] : rn ? '0 seconds' : '', // reset-now 表示立即重置
        };
      })
      .map((s) => ({ ...s, usedPct: clampPct(parseFloat(s.value)) }))
      .filter((s) => s.usedPct != null);
    let order = 0;
    for (const s of slots) {
      const byLabel = /rolling|5\s*hour|滚动|5\s*小时/i.test(s.label) ? '5h'
        : /weekly|week|每周/i.test(s.label) ? 'week'
        : /monthly|month|每月/i.test(s.label) ? 'month'
        : null;
      const key = byLabel || (order < SSR_NAMES.length ? SSR_NAMES[order][1] : null);
      order++;
      if (!key || out[key]) continue;
      const sec = parseHumanDuration(s.reset);
      out[key] = { usedPct: s.usedPct, resetsAt: sec == null ? null : Date.now() + sec * 1000 };
    }
  }
  return out;
}

/**
 * 官方用量查询：GET https://opencode.ai/workspace/{workspaceId}/go（带登录 Cookie）
 * 返回 { ok, items: [{ key, label, limit, spent, remaining, usedPct, resetsAt, currency, official }], ... }
 */
async function probeQuota(source) {
  const workspaceId = String(source.workspaceId || '').trim();
  let cookie = String(source.authCookie || '').trim();
  if (!workspaceId || !cookie) {
    return { ok: false, error: '未配置工作区 ID / Auth Cookie（设置 → OpenCode Go → 官方用量查询）' };
  }
  if (!/^auth=/i.test(cookie)) cookie = 'auth=' + cookie; // 容忍直接粘贴 Cookie 值
  const url = `https://opencode.ai/workspace/${encodeURIComponent(workspaceId)}/go`;
  let res;
  try {
    res = await getJson(url, { Cookie: cookie }, 15000);
  } catch (e) {
    return { ok: false, error: `请求失败: ${e.message}` };
  }
  // 未登录会被 302 到 /auth/authorize
  if (res.status === 302 || res.status === 401 || /\/auth\/authorize/.test((res.headers && res.headers.location) || '')) {
    return { ok: false, error: '会话已过期或 Cookie 无效，请重新登录 opencode.ai 后复制最新 auth Cookie' };
  }
  if (res.status >= 400) return { ok: false, error: errMsg(res) };
  const body = typeof res.body === 'string' ? res.body : String(res.raw || res.body || '');
  const usage = extractUsage(body);
  if (!usage['5h'] && !usage.week && !usage.month) {
    return { ok: false, error: '页面已返回但未解析到用量数据（页面结构可能已变更）' };
  }
  const items = [];
  for (const lim of LIMITS) {
    const u = usage[lim.key];
    if (!u) continue;
    const spent = round4((lim.limitUsd * u.usedPct) / 100);
    items.push({
      key: lim.key,
      label: lim.label,
      limit: lim.limitUsd,
      spent,
      remaining: Math.max(0, round4(lim.limitUsd - spent)),
      usedPct: u.usedPct,
      resetsAt: u.resetsAt || null,
      currency: 'USD',
      official: true,
    });
  }
  if (!items.length) return { ok: false, error: '未解析到有效用量项' };
  return { ok: true, sourceId: source.id, mode: 'opencode-go-official', items, currency: 'USD', official: true };
}

/** 依据历史记录计算三个限额窗口的剩余%（动态计算，无需请求） */
function computeQuota({ history, source }) {
  const now = Date.now();
  const items = [];

  for (const lim of LIMITS) {
    let spent = 0, partial = false, firstTs = null, resetsAt = null;
    if (lim.windowMs > 0) {
      const w = history.spentInWindow({ sourceId: source.id, ms: lim.windowMs });
      spent = w.spent;
      partial = w.partial;
      firstTs = w.firstTs;
      if (firstTs != null) resetsAt = firstTs + lim.windowMs;
    } else {
      const wm = history.spentInCalendarMonth({ sourceId: source.id });
      spent = wm.spent;
      partial = wm.partial;
      resetsAt = monthEndTs(now);
    }
    const remaining = Math.max(0, lim.limitUsd - spent);
    items.push({
      key: lim.key,
      label: lim.label,
      limit: lim.limitUsd,
      spent: spent,
      remaining: remaining,
      usedPct: lim.limitUsd ? Math.min(100, (spent / lim.limitUsd) * 100) : 0,
      resetsAt,
      currency: 'USD',
      partial,
    });
  }

  return { sourceId: source.id, mode: 'opencode-go-limits', items, currency: 'USD', local: true };
}

/** 设置界面：测试官方用量查询（已配置时）或代理配置 */
async function test(source) {
  const official = source.workspaceId && source.authCookie;
  if (official) {
    const r = await probeQuota(source);
    if (r.ok) {
      const parts = r.items.map((i) => `${i.label} 已用 ${i.usedPct.toFixed(1)}% / 剩余 $${i.remaining.toFixed(2)}`);
      return { ok: true, message: `官方用量: ${parts.join('；')}` };
    }
    return { ok: false, error: `官方用量查询失败: ${r.error}` };
  }
  const msgs = [];
  if (!source.port) msgs.push('未配置代理端口');
  const base = (source.baseUrl || 'https://opencode.ai/zen/go').replace(/\/+$/, '');
  msgs.push(`代理目标: ${base}`);
  msgs.push(`限额: 5h $12 / 周 $30 / 月 $60`);
  msgs.push('启动代理后，将客户端 API Base URL 指向 http://127.0.0.1:' + (source.port || '—'));
  msgs.push('配置「工作区 ID + Auth Cookie」可获取官方剩余用量（见下方字段）');
  return { ok: true, message: msgs.join('；') };
}

module.exports = { DEFAULT_PRICE_MAP, LIMITS, costFor, computeQuota, probeQuota, extractUsage, test, modelKey };
