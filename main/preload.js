'use strict';
// 预加载脚本：通过 contextBridge 暴露安全 API（两个窗口共用）

const { contextBridge, ipcRenderer } = require('electron');

const api = {
  // ---- 主界面 ----
  getSnapshot: (opts) => ipcRenderer.invoke('data:snapshot', opts),
  onUpdate: (cb) => {
    const listener = (_e, d) => cb(d);
    ipcRenderer.on('data:update', listener);
    return () => ipcRenderer.removeListener('data:update', listener);
  },
  onOpacity: (cb) => {
    const listener = (_e, v) => cb(v);
    ipcRenderer.on('app:opacity', listener);
    return () => ipcRenderer.removeListener('app:opacity', listener);
  },
  onFocus: (cb) => {
    const listener = (_e, f) => cb(f);
    ipcRenderer.on('app:focus', listener);
    return () => ipcRenderer.removeListener('app:focus', listener);
  },
  resizeWindow: (h) => ipcRenderer.invoke('win:resize', h),
  getPinned: () => ipcRenderer.invoke('win:get-pinned'),
  setPinned: (v) => ipcRenderer.invoke('win:set-pinned', v),
  openSettings: () => ipcRenderer.invoke('win:open-settings'),
  toggleWindow: () => ipcRenderer.invoke('win:toggle'),
  quit: () => ipcRenderer.invoke('app:quit'),
  winMinimize: () => ipcRenderer.invoke('win:minimize'),
  winClose: () => ipcRenderer.invoke('win:close'),

  // ---- 设置 ----
  getConfig: () => ipcRenderer.invoke('config:get'),
  saveConfig: (cfg) => ipcRenderer.invoke('config:save', cfg),
  testSource: (cfg) => ipcRenderer.invoke('source:test', cfg),
  refreshNow: () => ipcRenderer.invoke('source:refresh-all'),
  rescanTools: () => ipcRenderer.invoke('detect:scan'),
  getAutoLaunch: () => ipcRenderer.invoke('autolaunch:get'),
  setAutoLaunch: (v) => ipcRenderer.invoke('autolaunch:set', v),
  clearData: () => ipcRenderer.invoke('data:clear'),
  exportData: () => ipcRenderer.invoke('data:export'),
  proxyStatus: () => ipcRenderer.invoke('proxy:status'),
  proxyStart: () => ipcRenderer.invoke('proxy:start'),
  proxyStop: () => ipcRenderer.invoke('proxy:stop'),

  platform: process.platform,
};

// 注意：Electron 会把 exposeInMainWorld 的 key 注册为页面全局词法绑定，
// 因此不能用 'api' 这类常见标识符（会与页面 const api 声明冲突）
contextBridge.exposeInMainWorld('TW', api);
