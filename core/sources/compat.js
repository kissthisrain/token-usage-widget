'use strict';
// OpenAI 兼容中转站适配器：余额/用量接口自动探测 + 自定义 URL/JSON 映射兜底
// 常见余额接口形态：
//  - openai-billing:  GET {base}/v1/dashboard/billing/usage?start_date=&end_date=  → {total_usage(分), total_granted}
//  - deepseek-balance:GET {base}/v1/user/balance                                   → {balance_infos:[{currency,total_balance,granted_balance,topped_up_balance}]}
//  - openrouter-key:  GET {base}/api/v1/auth/key                                   → {data:{usage,limit}}
//  - one-api:         GET {base}/api/user/self                                     → {data:{quota,used_quota}}
//  - custom:          用户指定 URL + JSON 字段路径映射

const { getJson, errMsg } = require('../http');

const PROBE_MODES = ['openai-billing', 'deepseek-balance', 'openrouter-key', 'one-api'];

function authHeaders(source) {
  const h = { Authorization: `Bearer ${source.apiKey}` };
  return h;
}

function oneDayRange() {
  const d = new Date(Date.now() - 86400000);
  const f = (n) => String(n).padStart(2, '0');
  const end = new Date();
  return {
    start_date: `${d.getFullYear()}-${f(d.getMonth() + 1)}-${f(d.getDate())}`,
    end_date: `${end.getFullYear()}-${f(end.getMonth() + 1)}-${f(end.getDate())}`,
  };
}

/** 从响应推断额度（通用解析，容错） */
function inferQuota(json, mode) {
  const items = [];
  const pick = (o, ...paths) => {
    for (const p of paths) {
      const v = p.split('.').reduce((acc, k) => (acc == null ? undefined : acc[k]), o);
      if (v !== undefined) return v;
    }
    return undefined;
  };
  const num = (v) => (v == null || !isFinite(Number(v)) ? null : Number(v));
  const data = json && typeof json === 'object' ? json.data || json : json;
  const isObj = (x) => x != null && typeof x === 'object';

  // 1. balance_infos（deepseek 风格）
  const infos = json.balance_infos || (isObj(data) && data.balance_infos);
  if (Array.isArray(infos) && infos.length) {
    for (const b of infos) {
      items.push({
        label: b.currency || '余额',
        remaining: num(b.total_balance),
        limit: num(b.total_balance),
        usedPct: null,
        currency: b.currency || 'USD',
        mode: 'balance',
      });
    }
    return { items, matched: true };
  }

  // 2. openrouter: data.usage / data.limit
  const usage = pick(data, 'usage'), limit = pick(data, 'limit');
  if (usage != null && limit != null) {
    const used = num(usage), lim = num(limit);
    items.push({
      label: 'Key 额度',
      remaining: lim != null && used != null ? Math.max(0, lim - used) : null,
      limit: lim,
      usedPct: lim ? Math.min(100, ((used || 0) / lim) * 100) : null,
      currency: 'USD',
      mode: 'openrouter',
    });
    return { items, matched: true };
  }

  // 3. one-api: data.quota / data.used_quota（积分制）
  const quota = pick(data, 'quota'), usedQuota = pick(data, 'used_quota');
  if (quota != null || usedQuota != null) {
    const rem = num(quota), used = num(usedQuota);
    const lim = rem != null && used != null ? rem + used : null;
    items.push({
      label: '额度',
      remaining: rem,
      limit: lim,
      usedPct: lim ? Math.min(100, ((used || 0) / lim) * 100) : null,
      currency: 'USD',
      mode: 'one-api',
      note: 'one-api 积分额度（非美元）',
    });
    return { items, matched: true };
  }

  // 4. openai billing: total_usage（分） + total_granted
  const totalUsage = pick(json, 'total_usage', 'data.total_usage');
  if (totalUsage != null) {
    const used = num(totalUsage) / 100;
    const granted = num(pick(json, 'total_granted', 'data.total_granted'));
    items.push({
      label: '用量',
      remaining: granted != null ? Math.max(0, granted - used) : null,
      limit: granted,
      usedPct: granted ? Math.min(100, (used / granted) * 100) : null,
      currency: 'USD',
      mode: 'billing',
    });
    return { items, matched: true };
  }

  return { items, matched: false };
}

function buildCandidateUrls(source) {
  const base = (source.baseUrl || '').replace(/\/+$/, '');
  const range = oneDayRange();
  const urls = [];
  if (source.balanceMode === 'custom' && source.customBalanceUrl) {
    urls.push({ mode: 'custom', url: source.customBalanceUrl });
  }
  if (source.balanceMode === 'auto' || source.balanceMode === 'openai-billing') {
    urls.push({ mode: 'openai-billing', url: `${base}/v1/dashboard/billing/usage?start_date=${range.start_date}&end_date=${range.end_date}` });
  }
  if (source.balanceMode === 'auto' || source.balanceMode === 'deepseek-balance') {
    urls.push({ mode: 'deepseek-balance', url: `${base}/v1/user/balance` });
    urls.push({ mode: 'deepseek-balance', url: `${base}/user/balance` });
  }
  if (source.balanceMode === 'auto' || source.balanceMode === 'openrouter-key') {
    urls.push({ mode: 'openrouter-key', url: `${base}/api/v1/auth/key` });
  }
  if (source.balanceMode === 'auto' || source.balanceMode === 'one-api') {
    urls.push({ mode: 'one-api', url: `${base}/api/user/self` });
  }
  return urls;
}

/** 探测并获取额度（auto 模式按候选顺序尝试） */
async function probe(source) {
  const candidates = buildCandidateUrls(source);
  if (!candidates.length) {
    return { ok: false, error: '没有可探测的接口：请配置 Base URL 或自定义余额接口' };
  }
  const base = (source.baseUrl || '').replace(/\/+$/, '');
  let lastErr = null;
  for (const { mode, url } of candidates) {
    try {
      const res = await getJson(url, authHeaders(source));
      if (res.status === 401 || res.status === 403) {
        lastErr = new Error(`鉴权失败 (${mode}): ${errMsg(res)}`);
        continue;
      }
      if (res.status >= 400) {
        lastErr = new Error(`请求失败 (${mode}): ${errMsg(res)}`);
        continue;
      }
      const body = typeof res.body === 'string' ? safeParse(res.body) : res.body;
      const { items, matched } = inferQuota(body, mode);
      if (matched && items.length) {
        return {
          ok: true,
          mode,
          matched: true,
          items,
          message: `探测成功：接口 ${mode}，${items.length} 项额度信息`,
          baseUrl: base,
        };
      }
      lastErr = new Error(`接口 ${mode} 返回了数据但未能解析出额度字段`);
    } catch (e) {
      lastErr = e;
    }
  }
  return { ok: false, mode: null, matched: false, error: lastErr ? lastErr.message : '所有接口均不可用' };
}

function safeParse(s) {
  try { return JSON.parse(s); } catch { return {}; }
}

/** 测试连接（设置界面用） */
async function test(source) {
  const r = await probe(source);
  if (r.ok) {
    const parts = r.items.map((i) => `${i.label}: ${i.remaining != null ? i.remaining : '?'}${i.currency === 'USD' ? '$' : ' ' + (i.currency || '')}`).join('；');
    return { ok: true, message: r.message + (parts ? `（${parts}）` : ''), ...r };
  }
  return r;
}

module.exports = { probe, test, inferQuota, PROBE_MODES };
