'use strict';
// 工具自动探测：扫描已知路径，识别 ZCode / Codex / OpenCode CLI 等本地记录

const fs = require('fs');
const os = require('os');
const path = require('path');
const { listJsonlFiles, parseFile, scanParamsForSource } = require('./sources/local-logs');
const { dateKey, parseTs } = require('./util');

const home = os.homedir();

// 探测档案：新增工具只需加一条
const PROFILES = [
  {
    id: 'zcode',
    name: 'ZCode',
    tool: 'zcode',
    paths: [path.join(home, '.zcode', 'cli', 'rollout')],
    recursive: false,
    live: true,
    desc: 'ZCode 客户端会话记录（实时写入）',
  },
  {
    id: 'codex',
    name: 'Codex',
    tool: 'codex',
    paths: [path.join(home, '.codex', 'sessions')],
    recursive: true,
    live: false,
    desc: 'Codex CLI 会话记录（含历史用量与周配额）',
  },
  {
    id: 'opencode',
    name: 'OpenCode CLI',
    tool: 'zcode',
    paths: [
      path.join(home, '.local', 'share', 'opencode'),
      path.join(home, '.config', 'opencode'),
    ],
    recursive: true,
    live: true,
    desc: 'OpenCode CLI 会话记录（如已安装）',
  },
];

const SCAN_FILE_LIMIT = 800; // 单工具扫描文件上限，防止巨量文件拖慢

function dirExists(p) {
  try { return fs.statSync(p).isDirectory(); } catch { return false; }
}

/**
 * 扫描单个档案 → 画像
 * @returns {null | {id,name,tool,path,fileCount,recordCount,dateFrom,dateTo,models,live,exists}}
 */
function scanProfile(profile) {
  const dir = profile.paths.find(dirExists);
  if (!dir) {
    return { id: profile.id, name: profile.name, tool: profile.tool, exists: false, live: profile.live };
  }
  const params = scanParamsForSource({ tool: profile.tool });
  let files = listJsonlFiles(dir, profile.recursive, params.pattern);
  files = files.slice(-SCAN_FILE_LIMIT);

  let recordCount = 0;
  let dateFrom = null, dateTo = null;
  const models = new Set();

  for (const f of files) {
    const { records, rateLimits } = parseFile(f, params.format);
    for (const r of records) {
      recordCount++;
      const ts = r.ts || Date.now();
      const k = dateKey(ts);
      if (!dateFrom || k < dateFrom) dateFrom = k;
      if (!dateTo || k > dateTo) dateTo = k;
      if (r.model) models.add(r.model);
    }
    if (rateLimits && rateLimits.length) models.add('rate_limits');
    if (files.length > 60 && recordCount > 50000) break; // 足够画像，避免过慢
  }

  return {
    id: profile.id,
    name: profile.name,
    tool: profile.tool,
    exists: true,
    path: dir,
    fileCount: files.length,
    recordCount,
    dateFrom: dateFrom || null,
    dateTo: dateTo || null,
    models: [...models].sort(),
    live: profile.live,
    desc: profile.desc,
  };
}

/** 扫描全部档案 */
function scanAll() {
  const results = [];
  for (const p of PROFILES) {
    try { results.push(scanProfile(p)); } catch (e) {
      results.push({ id: p.id, name: p.name, tool: p.tool, exists: false, error: e.message });
    }
  }
  return results;
}

module.exports = { PROFILES, scanAll, scanProfile };
