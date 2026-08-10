'use strict';
// 一次性恢复：修复 watcher 卡死导致的漏计
// 1. 清理已删除源(孤儿)的历史数据
// 2. 重置本地记录源的文件偏移 → 下次启动全量重扫
// 3. 清理今天的数据避免重复（由全量重扫重建）
const fs = require('fs');

const DATA_DIR = 'C:/Users/kiss/AppData/Roaming/token-usage-widget';
const h = JSON.parse(fs.readFileSync(DATA_DIR + '/history.json', 'utf8'));

// 1. 清理孤儿源（config 中不存在的本地记录源）
let cfg = null;
try { cfg = JSON.parse(fs.readFileSync(DATA_DIR + '/config.json', 'utf8')); } catch { /* noop */ }
const validLocalIds = new Set(
  (cfg && cfg.sources || []).filter((s) => s.type === 'local-logs').map((s) => s.id)
);
let removedKeys = 0;
for (const k of Object.keys(h.days || {})) {
  for (const sid of Object.keys(h.days[k])) {
    if (sid.startsWith('src_') && !validLocalIds.has(sid) && !sid.startsWith('smoke_')) {
      delete h.days[k][sid];
      removedKeys++;
    }
  }
}
console.log('清理孤儿源数据条目:', removedKeys);

// 2. 重置本地记录源的偏移
for (const sid of Object.keys(h.fileOffsets || {})) {
  if (validLocalIds.has(sid)) {
    const before = JSON.stringify(h.fileOffsets[sid]).length;
    h.fileOffsets[sid] = {};
    console.log(`重置偏移: ${sid} (原 ${before} 字节索引)`);
  }
}

// 3. 清除今天的数据（全量重扫时重建，避免重复）
const p = (n) => String(n).padStart(2, '0');
const d = new Date();
const today = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
if (h.days[today]) {
  for (const sid of validLocalIds) {
    if (h.days[today][sid]) {
      delete h.days[today][sid];
      console.log(`清除今天 ${sid} 的数据（待重扫重建）`);
    }
  }
}

fs.writeFileSync(DATA_DIR + '/history.json', JSON.stringify(h));
console.log('恢复完成');
