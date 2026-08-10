'use strict';
// OpenAI 官方用量 API 适配器
// - 新接口: GET {base}/v1/usage?start_time=&end_time=&bucket_width=1d&group_by[]=model
// - 旧接口: GET {base}/v1/organization/usage/completions?...
// 需要「组织管理员」API Key（普通 key 无法查询用量，会返回 403）
// 返回按天分桶的记录，可直接回填历史趋势

const { getJson, errMsg } = require('../http');
const { dateKey, parseTs } = require('../util');

function buildUrls(base, start, end) {
  const q = `start_time=${start}&end_time=${end}&bucket_width=1d&group_by[]=model`;
  return [
    `${base}/v1/usage?${q}`,
    `${base}/v1/organization/usage/completions?${q}`,
  ];
}

function authHeaders(source) {
  const h = { Authorization: `Bearer ${source.apiKey}` };
  if (source.organizationId) h['OpenAI-Organization'] = source.organizationId;
  return h;
}

/** 解析两种接口的响应 → 归一化记录列表 */
function parseUsagePayload(payload) {
  const records = [];
  const buckets = payload.data || [];
  for (const bucket of buckets) {
    const ts = parseTs(bucket.start_time ?? bucket.startTime);
    if (!ts) continue;
    const results = bucket.results || [];
    for (const r of results) {
      const input = r.input_tokens ?? r.inputTokens ?? null;
      const output = r.output_tokens ?? r.outputTokens ?? null;
      const total = r.total_tokens ?? r.totalTokens ?? (input != null && output != null ? input + output : null);
      if (total == null) continue;
      const costRaw = r.cost;
      const cost = costRaw != null && typeof costRaw === 'object' ? costRaw.amount : costRaw;
      const currency = costRaw != null && typeof costRaw === 'object' ? costRaw.currency : 'USD';
      records.push({
        ts,
        model: r.model || r.api_model || r.model_id || null,
        input: input ?? null,
        output: output ?? null,
        total,
        cost: cost != null && isFinite(cost) ? cost : null,
        currency,
      });
    }
  }
  return records;
}

/** 拉取最近 N 天用量 */
async function fetchUsage(source, { days = 90 } = {}) {
  const base = (source.baseUrl || 'https://api.openai.com').replace(/\/+$/, '');
  const end = Math.floor(Date.now() / 1000);
  const start = end - days * 86400;
  const urls = buildUrls(base, start, end);
  let lastErr = null;
  for (const url of urls) {
    try {
      const res = await getJson(url, authHeaders(source));
      if (res.status >= 200 && res.status < 300) {
        return parseUsagePayload(res.body);
      }
      lastErr = new Error(errMsg(res));
      if (res.status === 403) {
        lastErr = new Error('用量接口返回 403：需要「组织管理员」API Key（在 platform.openai.com → Organization → API keys 创建），普通 Key 无用量权限');
        break;
      }
      if (res.status !== 404) break; // 非接口不存在错误 → 停止尝试
    } catch (e) {
      lastErr = e;
      break;
    }
  }
  throw lastErr || new Error('OpenAI 用量接口请求失败');
}

/** 测试连接 */
async function test(source) {
  try {
    const records = await fetchUsage(source, { days: 3 });
    const days = new Set(records.map((r) => dateKey(r.ts)));
    return {
      ok: true,
      message: `连接成功：3 天内 ${records.length} 条模型用量记录，覆盖 ${days.size} 天`,
      records: records.length,
      days: [...days].sort().slice(-3),
    };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

module.exports = { fetchUsage, test };
