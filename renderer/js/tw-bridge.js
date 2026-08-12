'use strict';
// Tauri 桥接层：将 window.__TAURI__ 封装为与原 Electron preload 一致的 window.TW API
// 使 renderer/js/widget.js 与 settings.js 无需改动即可对接 Tauri 后端
// 事件订阅（listen 返回 Promise<UnlistenFn>）在桥接层收敛为同步返回取消函数

(function () {
  const T = window.__TAURI__;
  if (!T) {
    console.error('[tw-bridge] window.__TAURI__ 不可用（tauri 注入失败？）');
    window.TW = null;
    return;
  }
  const invoke = (cmd, args) => T.core.invoke(cmd, args);

  const api = {
    // ---- 主界面 ----
    getSnapshot: (opts) => invoke('get_snapshot', { opts: opts || {} }),
    onUpdate: (cb) => subscribe('data:update', cb),
    onOpacity: (cb) => subscribe('app:opacity', cb),
    onFocus: (cb) => subscribe('app:focus', cb),
    onDock: (cb) => subscribe('app:dock', cb),
    showMini: () => invoke('win_show_mini'),
    showMain: () => invoke('win_show_main'),
    resizeWindow: (h) => invoke('win_resize', { h: Number(h) }),
    getPinned: () => invoke('win_get_pinned'),
    setPinned: (v) => invoke('win_set_pinned', { v: !!v }),
    openSettings: () => invoke('win_open_settings'),
    toggleWindow: () => invoke('win_toggle'),
    quit: () => invoke('app_quit'),
    winMinimize: () => invoke('win_minimize'),
    winClose: () => invoke('win_close'),

    // ---- 设置 ----
    getConfig: () => invoke('get_config'),
    saveConfig: (cfg) => invoke('save_config', { cfg }),
    testSource: (cfg) => invoke('test_source', { cfg }),
    refreshNow: () => invoke('refresh_all'),
    rescanTools: () => invoke('detect_scan'),
    getAutoLaunch: () => invoke('get_autolaunch'),
    setAutoLaunch: (v) => invoke('set_autolaunch', { v: !!v }),
    clearData: () => invoke('clear_data'),
    exportData: () => invoke('export_data'),
    proxyStatus: () => invoke('proxy_status'),
    proxyStart: () => invoke('proxy_start'),
    proxyStop: () => invoke('proxy_stop'),

    platform: 'win32',

    // ---- 原生对话框（替换 window.confirm / window.alert，Tauri 默认禁用脚本对话框） ----
    ask: (msg, title) => T.dialog.ask(msg, { title: title || '确认', kind: 'warning' }),
    alert: (msg, title) => T.dialog.message(msg, { title: title || '提示', kind: 'info' }),
  };

  function subscribe(eventName, cb) {
    // Tauri listen 返回 Promise<UnlistenFn>；同步返回取消函数，与 Electron API 对齐
    let unlisten = null;
    T.event.listen(eventName, (e) => cb(e.payload)).then((fn) => { unlisten = fn; });
    return () => { if (unlisten) unlisten(); };
  }

  window.TW = api;
})();
