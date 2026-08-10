'use strict';
// OpenCode Go 本地代理
// 客户端把 API Base URL 指向 http://127.0.0.1:<port>，本代理转发到 opencode.ai/zen/go
// 从每次响应提取 usage（兼容 OpenAI chat.completions / responses / Anthropic messages 三种字段风格），
// 结合官方价目表计算美元费用 → 写入历史 → 驱动配额剩余% 实时刷新

const http = require('http');
const https = require('https');
const { EventEmitter } = require('events');
const { costFor } = require('../core/sources/opencode-go');

class GoProxy extends EventEmitter {
  /**
   * @param {() => object} getSource 返回最新 opencode-go 源配置
   */
  constructor({ getSource, history, log }) {
    super();
    this.getSource = getSource;
    this.history = history;
    this.log = log || console;
    this.server = null;
    this.host = '127.0.0.1';
  }

  get source() {
    return this.getSource() || {};
  }

  isRunning() {
    return !!this.server && this.server.listening;
  }

  start() {
    if (this.isRunning()) return Promise.resolve(true);
    const target = new URL(this.baseUrl);
    this.server = http.createServer((req, res) => this.handle(req, res, target));
    return new Promise((resolve) => {
      this.server.once('error', (e) => {
        this.log.warn(`[proxy] 启动失败: ${e.message}`);
        this.server = null;
        resolve(false);
      });
      this.server.listen(this.port, this.host, () => {
        this.log.info(`[proxy] 本地代理已启动 http://${this.host}:${this.port} → ${this.baseUrl}`);
        resolve(true);
      });
    });
  }

  stop() {
    if (this.server) {
      try { this.server.close(); } catch { /* noop */ }
      this.server = null;
    }
  }

  get port() {
    return Number(this.source.port) || 12345;
  }

  get baseUrl() {
    return (this.source.baseUrl || 'https://opencode.ai/zen/go').replace(/\/+$/, '');
  }

  handle(req, res, target) {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const bodyBuf = Buffer.concat(chunks);
      let reqModel = null;
      try {
        const b = JSON.parse(bodyBuf.toString('utf8') || '{}');
        reqModel = b.model || null;
      } catch { /* 非 JSON 请求体 */ }

      const isStream = String(req.headers.accept || '').includes('text/event-stream');
      const url = target.origin + (req.url || '/');
      const headers = { ...req.headers };
      delete headers.host;
      delete headers.connection;
      delete headers['content-length'];
      headers.host = target.host;
      headers.accept = req.headers.accept || 'application/json';

      const lib = target.protocol === 'https:' ? https : http;
      const preq = lib.request(url, { method: req.method, headers }, (pres) => {
        res.writeHead(pres.statusCode, pres.headers);
        const resChunks = [];
        pres.on('data', (c) => {
          if (isStream) {
            res.write(c);
            this.parseStreamChunk(c, reqModel);
          } else {
            resChunks.push(c);
          }
        });
        pres.on('end', () => {
          if (!isStream) {
            this.recordFromJson(Buffer.concat(resChunks), reqModel);
          }
          try { res.end(); } catch { /* noop */ }
        });
        pres.on('error', () => { try { res.end(); } catch { /* noop */ } });
      });
      preq.on('error', (e) => {
        this.log.warn(`[proxy] 转发失败: ${e.message}`);
        try {
          if (!res.headersSent) {
            res.writeHead(502, { 'content-type': 'application/json' });
            res.end(JSON.stringify({ error: { message: `代理转发失败: ${e.message}` } }));
          } else {
            res.end();
          }
        } catch { /* noop */ }
      });
      preq.write(bodyBuf);
      preq.end();
    });
  }

  parseStreamChunk(chunk, model) {
    const text = chunk.toString('utf8');
    for (const line of text.split('\n')) {
      if (!line.startsWith('data:')) continue;
      const payload = line.slice(5).trim();
      if (!payload || payload === '[DONE]') continue;
      try {
        this.extractAndRecord(JSON.parse(payload), model);
      } catch { /* 半截 JSON 忽略 */ }
    }
  }

  recordFromJson(buf, model) {
    try {
      this.extractAndRecord(JSON.parse(buf.toString('utf8')), model);
    } catch { /* 非 JSON 响应 */ }
  }

  extractAndRecord(j, reqModel) {
    if (!j || typeof j !== 'object') return;
    const model = j.model || reqModel;
    if (j.usage) {
      this.recordUsage(j.usage, model);
      return;
    }
    // chat.completion.chunk / message 内嵌 usage
    const msg = j.choices && j.choices[0] && (j.choices[0].message || j.choices[0].delta);
    if (msg && msg.usage) this.recordUsage(msg.usage, model);
  }

  recordUsage(u, model) {
    const input = u.input_tokens ?? u.inputTokens ?? u.prompt_tokens ?? null;
    const output = u.output_tokens ?? u.outputTokens ?? u.completion_tokens ?? null;
    const total = u.total_tokens ?? u.totalTokens ?? (input != null && output != null ? input + output : null);
    if (total == null || !isFinite(total) || total <= 0) return;
    const cost = costFor(model, input, output, this.source.priceMap);
    this.history.addRecords(this.source.id, [{
      ts: Date.now(),
      model,
      input: input ?? null,
      output: output ?? null,
      total,
      cost,
    }]);
    this.emit('records', { sourceId: this.source.id, count: 1 });
  }

  status() {
    return {
      running: this.isRunning(),
      port: this.port,
      host: this.host,
      baseUrl: this.baseUrl,
      hint: `将客户端的 API Base URL 设置为 http://127.0.0.1:${this.port}（保留 /v1 路径）`,
    };
  }
}

module.exports = { GoProxy };
