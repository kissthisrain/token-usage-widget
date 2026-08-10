'use strict';
// ZCode 数据库历史导入：只读 db.sqlite 的 model_usage 表，回填历史用量
// - rollout JSONL 只保留当天，历史在 db.sqlite（已验证含 30+ 天数据）
// - 只导入「今天之前」的记录（当天由实时 rollout 监听负责，避免重复计数）
// - 按天幂等替换（setSourceDay），重复运行安全
// - 用 sql.js（纯 WASM）读取，无需原生模块，Electron 主进程可用

const fs = require('fs');
const path = require('path');
const os = require('os');
const { dateKey } = require('../util');

let sqlInit = null;

function initSql() {
  if (!sqlInit) {
    sqlInit = require('sql.js')({
      locateFile: (f) => path.join(__dirname, '..', '..', 'node_modules', 'sql.js', 'dist', f),
    });
  }
  return sqlInit;
}

function defaultZcodeDbPath() {
  return path.join(os.homedir(), '.zcode', 'cli', 'db', 'db.sqlite');
}

/**
 * 导入 ZCode model_usage 历史
 * @returns {Promise<{imported:number, days:number, skipped?:boolean, error?:string}>}
 */
async function importZcodeHistory({ source, history, log }) {
  const dbPath = source.sqliteDb || defaultZcodeDbPath();
  if (!dbPath || !fs.existsSync(dbPath)) return { imported: 0, days: 0, skipped: true };

  let SQL;
  try {
    SQL = await initSql();
  } catch (e) {
    log && log.warn(`[sqlite] sql.js 初始化失败: ${e.message}`);
    return { imported: 0, days: 0, error: e.message };
  }

  try {
    const buf = fs.readFileSync(dbPath);
    const db = new SQL.Database(buf);
    const todayStart = new Date();
    todayStart.setHours(0, 0, 0, 0);
    const stmt = db.prepare(
      'SELECT completed_at, model_id, input_tokens, output_tokens, computed_total_tokens ' +
      'FROM model_usage WHERE completed_at IS NOT NULL AND completed_at < ?'
    );
    stmt.bind([todayStart.getTime()]);
    const byDay = new Map();
    while (stmt.step()) {
      const r = stmt.getAsObject();
      const t = Number(r.completed_at);
      const total = Number(r.computed_total_tokens);
      if (!t || !isFinite(total) || total <= 0) continue;
      const day = dateKey(t);
      const m = byDay.get(day) || byDay.set(day, {}).get(day);
      const model = r.model_id || '(unknown)';
      const a = (m[model] ??= { tokens: 0, input: 0, output: 0, cost: 0, calls: 0 });
      a.tokens += total;
      a.input += Number(r.input_tokens) || 0;
      a.output += Number(r.output_tokens) || 0;
      a.calls += 1;
    }
    stmt.free();
    db.close();

    let imported = 0;
    for (const [day, models] of byDay) {
      history.setSourceDay(source.id, day, models);
      imported += Object.values(models).reduce((s, a) => s + a.calls, 0);
    }
    if (imported) {
      log && log.info(`[sqlite] ${source.name}: 回填 ${imported} 条历史（${byDay.size} 天）`);
    }
    return { imported, days: byDay.size };
  } catch (e) {
    log && log.warn(`[sqlite] ${source.name} 历史导入失败: ${e.message}`);
    return { imported: 0, days: 0, error: e.message };
  }
}

module.exports = { importZcodeHistory, defaultZcodeDbPath };
