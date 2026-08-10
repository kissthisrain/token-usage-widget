'use strict';
// 系统托盘

const { Tray, Menu, nativeImage } = require('electron');
const path = require('path');

function createTray({ onToggle, onSettings, onQuit }) {
  const icon = nativeImage.createFromPath(path.join(__dirname, '..', 'resources', 'icon.png'));
  const tray = new Tray(icon.isEmpty() ? nativeImage.createEmpty() : icon.resize({ width: 16, height: 16 }));
  tray.setToolTip('Token 用量小组件');
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: '显示 / 隐藏', click: onToggle },
    { label: '设置…', click: onSettings },
    { type: 'separator' },
    { label: '退出', click: onQuit },
  ]));
  tray.on('click', onToggle);
  return tray;
}

module.exports = { createTray };
