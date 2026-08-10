'use strict';
// Electron 主进程入口
// - 悬浮窗口 / 设置窗口 / 托盘 / 全局热键 / 开机自启
// - 启动本地记录监听（watcher）与 API 调度（scheduler）与 OpenCode Go 代理
// - IPC 桥接渲染进程

const { app, globalShortcut, ipcMain, dialog } = require('electron');
const path = require('path');
const fs = require('fs');

const { ConfigStore } = require('../core/config');
const { HistoryStore } = require('../core/history');
const { Watcher } = require('../core/watcher');
const { Scheduler } = require('../core/scheduler');
const { buildSnapshot } = require('../core/snapshot');
const { scanAll } = require('../core/detect');
const { GoProxy } = require('./proxy');
const { WindowManager } = require('./window');
const { createTray } = require('./tray');
const openaiSrc = require('../core/sources/openai');
const compatSrc = require('../core/sources/compat');
const goSrc = require('../core/sources/opencode-go');

const SMOKE = process.argv.includes('--smoke');
const SHOT_PATH = (() => {
  const i = process.argv.indexOf('--shot');
  return i >= 0 ? process.argv[i + 1] : null;
})();

const userDataDir = app.getPath('userData');
const configStore = new ConfigStore(userDataDir);
const history = new HistoryStore(userDataDir).load();

let winMgr = null;
let tray = null;
let watcher = null;
let scheduler = null;
let goProxy = null;
let lastDetections = null;

const log = {
  info: (...a) => console.log('[info]', ...a),
  warn: (...a) => console.warn('[warn]', ...a),
};

function broadcast(channel, data) {
  const { BrowserWindow } = require('electron');
  for (const w of BrowserWindow.getAllWindows()) {
    if (!w.isDestroyed()) w.webContents.send(channel, data);
  }
}

function goSourceConfig() {
  return configStore.getSources().find((s) => s.type === 'opencode-go') || null;
}

// ---------- 核心服务 ----------

function initCore() {
  configStore.load();
  watcher = new Watcher({ history, log });
  watcher.setSourceProvider(() => configStore.getSources());
  watcher.on('records', (d) => broadcast('data:update', d));
  watcher.on('quota', (d) => broadcast('data:update', d));
  watcher.restart(configStore.getSources());

  scheduler = new Scheduler({ config: configStore, history, log });
  scheduler.on('usage', (d) => broadcast('data:update', d));
  scheduler.on('quota', (d) => broadcast('data:update', d));
  scheduler.start();

  goProxy = new GoProxy({ getSource: goSourceConfig, history, log });
  goProxy.on('records', (d) => broadcast('data:update', d));
  if (goSourceConfig() && goSourceConfig().enabled) goProxy.start();
}

function reinitCore() {
  watcher.restart(configStore.getSources());
  scheduler.start();
  const cfg = goSourceConfig();
  if (cfg && cfg.enabled && !goProxy.isRunning()) goProxy.start();
  if ((!cfg || !cfg.enabled) && goProxy.isRunning()) goProxy.stop();
  registerHotkey();
  applyAutoLaunch();
  if (winMgr) winMgr.applyOpacity();
  broadcast('data:update', { reinit: true });
}

// ---------- 热键 / 自启 ----------

function registerHotkey() {
  globalShortcut.unregisterAll();
  const hk = configStore.getGeneral().hotkey;
  if (!hk) return;
  try {
    globalShortcut.register(hk, () => winMgr && winMgr.toggleWidget());
  } catch (e) {
    log.warn(`热键注册失败 ${hk}: ${e.message}`);
  }
}

function applyAutoLaunch() {
  try {
    const v = !!configStore.getGeneral().autoLaunch;
    app.setLoginItemSettings({ openAtLogin: v, args: [] });
  } catch (e) {
    log.warn(`开机自启设置失败: ${e.message}`);
  }
}

// ---------- IPC ----------

function registerIpc() {
  ipcMain.handle('data:snapshot', (_e, opts) => {
    return buildSnapshot({
      config: configStore,
      history,
      range: (opts && opts.range) || 'month',
      sourceId: (opts && opts.sourceId) || 'all',
    });
  });

  ipcMain.handle('config:get', () => configStore.load());
  ipcMain.handle('config:save', (_e, cfg) => {
    if (!cfg || typeof cfg !== 'object') return { ok: false, error: '无效配置' };
    configStore.data = cfg;
    configStore.save();
    reinitCore();
    return { ok: true };
  });

  ipcMain.handle('source:test', async (_e, cfg) => {
    if (!cfg || !cfg.type) return { ok: false, error: '配置不完整' };
    try {
      if (cfg.type === 'openai') return await openaiSrc.test(cfg);
      if (cfg.type === 'compat') return await compatSrc.test(cfg);
      if (cfg.type === 'opencode-go') return await goSrc.test(cfg);
      if (cfg.type === 'local-logs') {
        const p = cfg.path || '';
        const ok = p && fs.existsSync(p);
        return ok
          ? { ok: true, message: `路径存在：${p}` }
          : { ok: false, error: `路径不存在：${p || '(未填写)'}` };
      }
      return { ok: false, error: `未知类型：${cfg.type}` };
    } catch (e) {
      return { ok: false, error: e.message };
    }
  });

  ipcMain.handle('source:refresh-all', async () => {
    await scheduler.tick();
    return { ok: true };
  });

  ipcMain.handle('detect:scan', () => {
    lastDetections = scanAll();
    return lastDetections;
  });

  ipcMain.handle('autolaunch:get', () => {
    try { return app.getLoginItemSettings().openAtLogin; } catch { return false; }
  });
  ipcMain.handle('autolaunch:set', (_e, v) => {
    try {
      app.setLoginItemSettings({ openAtLogin: !!v, args: [] });
      configStore.data.general.autoLaunch = !!v;
      configStore.save();
      return { ok: true };
    } catch (e) {
      return { ok: false, error: e.message };
    }
  });

  ipcMain.handle('data:clear', () => {
    history.clear();
    broadcast('data:update', { reinit: true });
    return { ok: true };
  });

  ipcMain.handle('data:export', async (_e) => {
    const { saveDialog } = dialog;
    const def = path.join(app.getPath('documents'), `token-usage-export-${Date.now()}.json`);
    const r = await saveDialog({ defaultPath: def, filters: [{ name: 'JSON', extensions: ['json'] }] });
    if (r.canceled || !r.filePath) return { ok: false, error: '已取消' };
    const payload = {
      exportedAt: new Date().toISOString(),
      config: configStore.data,
      history: { days: history.days, recent: history.recent, quota: history.quota },
    };
    fs.writeFileSync(r.filePath, JSON.stringify(payload, null, 2));
    return { ok: true, path: r.filePath };
  });

  ipcMain.handle('proxy:status', () => goProxy.status());
  ipcMain.handle('proxy:start', async () => {
    const ok = await goProxy.start();
    return { ok, status: goProxy.status() };
  });
  ipcMain.handle('proxy:stop', () => {
    goProxy.stop();
    return { ok: true, status: goProxy.status() };
  });

  ipcMain.handle('win:open-settings', () => { winMgr.openSettings(); return { ok: true }; });
  ipcMain.handle('win:toggle', () => { winMgr.toggleWidget(); return { ok: true }; });
  ipcMain.handle('win:resize', (_e, h) => { winMgr.resizeToContent(Number(h)); return { ok: true }; });
  ipcMain.handle('win:get-pinned', () => !!configStore.getGeneral().alwaysOnTop);
  ipcMain.handle('win:set-pinned', (_e, v) => winMgr.setPinned(!!v));
  ipcMain.handle('win:minimize', (e) => {
    const { BrowserWindow } = require('electron');
    const w = BrowserWindow.fromWebContents(e.sender);
    if (w) w.minimize();
    return { ok: true };
  });
  ipcMain.handle('win:close', (e) => {
    const { BrowserWindow } = require('electron');
    const w = BrowserWindow.fromWebContents(e.sender);
    if (w) w.close();
    return { ok: true };
  });
  ipcMain.handle('app:quit', () => { app.quit(); return { ok: true }; });
}

// ---------- 冒烟测试（--smoke：无界面验证数据管道） ----------

async function runSmoke() {
  console.log('=== 环境 ===');
  console.log('app.name:', app.getName());
  console.log('userData:', app.getPath('userData'));
  console.log('config 数据源数:', configStore.load().sources.length, JSON.stringify(configStore.load().sources.map((s) => s.name)));
  const detections = scanAll();
  console.log('=== 工具探测 ===');
  for (const d of detections) {
    console.log(JSON.stringify(d));
  }

  // 临时源：用探测到的工具补全快照
  const config = configStore.load();
  const tempSources = [];
  if (!config.sources.length) {
    for (const d of detections) {
      if (d.exists && d.recordCount > 0) {
        tempSources.push({
          id: 'smoke_' + d.id,
          type: 'local-logs',
          tool: d.tool,
          name: d.name,
          path: d.path,
          enabled: true,
        });
      }
    }
  }
  const sources = tempSources.length ? tempSources : config.sources;

  // 解析全部本地文件（模拟 watcher 初始扫描）
  const { listJsonlFiles, parseFile, scanParamsForSource } = require('../core/sources/local-logs');
  for (const s of sources) {
    if (s.type !== 'local-logs') continue;
    const params = scanParamsForSource(s);
    const files = listJsonlFiles(s.path, params.recursive, params.pattern);
    let n = 0, rate = 0;
    for (const f of files) {
      const r = parseFile(f, params.format);
      if (r.records.length) {
        history.addRecords(s.id, r.records);
        n += r.records.length;
      }
      if (r.rateLimits && r.rateLimits.length) {
        rate += r.rateLimits.length;
        history.setQuota(s.id, { sourceId: s.id, items: [{ label: r.rateLimits[r.rateLimits.length - 1].label, ...r.rateLimits[r.rateLimits.length - 1] }], mode: 'local-rate-limits', local: true });
      }
    }
    console.log(`[smoke] ${s.name}: ${files.length} 文件, ${n} 条用量记录, ${rate} 条配额`);
  }

  const snap = buildSnapshot({ config: { getSources: (e) => sources.filter((s) => !e || s.enabled) }, history, range: 'month' });
  console.log('=== 快照 ===');
  console.log(JSON.stringify({
    totals: snap.totals,
    activeDays: snap.activeDays,
    modelCount: snap.models.length,
    topModels: snap.models.slice(0, 5),
    trendDays: snap.trend.length,
    heatmapDays: snap.heatmap.length,
    sources: snap.sources.map((s) => ({ id: s.id, name: s.name, tokens: s.tokens })),
    quota: snap.quota.map((q) => ({ mode: q.mode, items: q.items && q.items.map((i) => ({ label: i.label, usedPct: i.usedPct, resetsAt: i.resetsAt })) })),
  }, null, 2));
  app.exit(0);
}

// ---------- 启动 ----------

const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (winMgr) winMgr.showWidget();
  });

  app.whenReady().then(() => {
    app.setAppUserModelId('com.tokenusage.widget');
    if (SMOKE) {
      runSmoke();
      return;
    }
    initCore();
    registerIpc();
    winMgr = new WindowManager({ configStore, log });
    winMgr.createWidget();
    winMgr.applyOpacity();
    if (SHOT_PATH) {
      // 截图调试模式：等渲染完成后截取窗口 → 保存 → 退出
      // 用法: electron . --shot <path> [settings] [general] [total] [dump]
      //   settings 截设置窗口；general 切通用标签；total 切 TOTAL 视图；dump 输出布局调试信息
      const shotSettings = process.argv.includes('settings');
      const shotTab = process.argv.includes('general') ? 'general' : null;
      setTimeout(async () => {
        try {
          const win = shotSettings ? winMgr.openSettings() : winMgr.widget;
          await new Promise((r) => setTimeout(r, 1800));
          if (shotTab) {
            await win.webContents.executeJavaScript(
              `document.querySelector('.tab[data-tab="${shotTab}"]')?.click(); true`
            );
            await new Promise((r) => setTimeout(r, 400));
          }
          if (process.argv.includes('total') || process.argv.includes('day')) {
            const r = process.argv.includes('total') ? 'total' : 'day';
            await win.webContents.executeJavaScript(
              `document.querySelector('#pills .pill[data-range="${r}"]')?.click(); true`
            );
            await new Promise((r) => setTimeout(r, 800));
          }
          if (process.argv.includes('dump')) {
            const info = await win.webContents.executeJavaScript(`(() => {
              // 精确像素：趋势图绘图区域 vs 热力图格子区域
              const tc = window.__trendChart;
              const hc = window.__heatmapChart;
              if (!tc || !hc) return 'charts not exposed';
              const trect = document.getElementById('trendChart').getBoundingClientRect();
              const hrect = document.getElementById('heatmapChart').getBoundingClientRect();
              const tLeft = tc.convertToPixel({ xAxisIndex: 0 }, 0);
              const tRight = tc.convertToPixel({ xAxisIndex: 0 }, tc.getOption().xAxis[0].data.length - 1);
              const hLeft = hc.convertToPixel({ calendarIndex: 0 }, [hc.getOption().calendar[0].range[0], 0]);
              const hRight = hc.convertToPixel({ calendarIndex: 0 }, [hc.getOption().calendar[0].range[1], 6]);
              const cvs = document.querySelector('#trendChart canvas');
              const foot = document.querySelector('.foot');
              const cRect = document.getElementById('trendChart').getBoundingClientRect();
              return JSON.stringify({
                trendChartW: Math.round(trect.width),
                heatmapChartW: Math.round(hrect.width),
                trendPlot: { left: Math.round(tLeft), right: Math.round(tRight) },
                heatmapCells: { left: hLeft ? Math.round(hLeft.x) : null, right: hRight ? Math.round(hRight.x) : null },
                deltaLeft: hLeft ? Math.round(tLeft - hLeft.x) : null,
                deltaRight: hRight ? Math.round(tRight - hRight.x) : null,
                canvas: cvs ? { top: Math.round(cvs.getBoundingClientRect().top), h: Math.round(cvs.getBoundingClientRect().height), bottom: Math.round(cvs.getBoundingClientRect().bottom), styleH: getComputedStyle(cvs).height } : null,
                chartDiv: { top: Math.round(cRect.top), h: Math.round(cRect.height), bottom: Math.round(cRect.bottom) },
                footer: foot ? { top: Math.round(foot.getBoundingClientRect().top), h: Math.round(foot.getBoundingClientRect().height) } : null,
                windowH: window.innerHeight,
              });
            })()`);
            console.log('[dump]', info);
          }
          const img = await win.webContents.capturePage();
          const fs2 = require('fs');
          fs2.writeFileSync(SHOT_PATH, img.toPNG());
          console.log('[shot] 已保存:', SHOT_PATH);
        } catch (e) {
          console.error('[shot] 失败:', e.message);
        }
        app.exit(0);
      }, 2500);
    }
    tray = createTray({
      onToggle: () => winMgr.toggleWidget(),
      onSettings: () => winMgr.openSettings(),
      onQuit: () => app.quit(),
    });
    registerHotkey();
    applyAutoLaunch();
  });

  app.on('window-all-closed', () => {
    // 托盘驻留：不退出
  });

  app.on('will-quit', () => {
    globalShortcut.unregisterAll();
    if (watcher) watcher.stop();
    if (scheduler) scheduler.stop();
    if (goProxy) goProxy.stop();
  });
}
