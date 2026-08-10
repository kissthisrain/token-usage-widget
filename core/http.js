'use strict';
// 轻量 HTTP 客户端（主进程用，无第三方依赖）

const http = require('http');
const https = require('https');

function request(url, { method = 'GET', headers = {}, body = null, timeoutMs = 20000, redirects = 0 } = {}) {
  return new Promise((resolve, reject) => {
    const lib = url.startsWith('https:') ? https : http;
    const u = new URL(url);
    const opts = {
      method,
      headers: { 'User-Agent': 'token-usage-widget/0.1', ...headers },
      timeout: timeoutMs,
    };
    const req = lib.request(u, opts, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => {
        const buf = Buffer.concat(chunks);
        const ct = String(res.headers['content-type'] || '');
        let data = buf.toString('utf8');
        if (ct.includes('json')) {
          try { data = JSON.parse(data); } catch { /* 保留原文 */ }
        }
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location && redirects < 3) {
          const next = new URL(res.headers.location, u).toString();
          return request(next, { method, headers, body, timeoutMs, redirects: redirects + 1 })
            .then(resolve, reject);
        }
        resolve({ status: res.statusCode, headers: res.headers, body: data, raw: buf });
      });
    });
    req.on('timeout', () => { req.destroy(new Error(`请求超时 (${timeoutMs}ms): ${url}`)); });
    req.on('error', (e) => reject(new Error(`请求失败: ${e.message}`)));
    if (body != null) req.write(body);
    req.end();
  });
}

async function getJson(url, headers = {}, timeoutMs = 20000) {
  const res = await request(url, { headers, timeoutMs });
  return res;
}

function errMsg(res) {
  if (typeof res.body === 'object' && res.body != null) {
    const e = res.body.error || res.body.message || res.body.msg;
    if (typeof e === 'string') return `HTTP ${res.status}: ${e.slice(0, 200)}`;
    if (e && e.message) return `HTTP ${res.status}: ${String(e.message).slice(0, 200)}`;
    return `HTTP ${res.status}`;
  }
  return `HTTP ${res.status}`;
}

module.exports = { request, getJson, errMsg };
