'use strict';
// 窗口管理：悬浮小组件窗口 + 设置窗口

const { BrowserWindow, Menu, screen, shell } = require('electron');
const path = require('path');

const RENDERER = path.join(__dirname, '..', 'renderer');
const PRELOAD = path.join(__dirname, 'preload.js');

class WindowManager {
  constructor({ configStore, log }) {
    this.configStore = configStore;
    this.log = log || console;
    this.widget = null;
    this.settings = null;
  }

  webPrefs() {
    return {
      preload: PRELOAD,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      backgroundThrottling: false,
      spellcheck: false,
    };
  }

  /** 窗口高度自适应内容（上限为屏幕工作区高度） */
  resizeToContent(height) {
    if (!this.widget || this.widget.isDestroyed()) return;
    try {
      const wa = screen.getDisplayNearestPoint(this.widget.getBounds()).workArea;
      const max = wa.height - 24;
      const h = Math.max(320, Math.min(Math.round(height), max));
      const [w] = this.widget.getSize();
      const [, curH] = this.widget.getContentSize();
      if (Math.abs(curH - h) > 3) {
        this.widget.setContentSize(w, h);
      }
    } catch { /* 窗口未就绪 */ }
  }

  /** 应用不透明度（透明窗口模式下 win.setOpacity 在 Windows 不生效，交给渲染层 CSS 实现） */
  applyOpacity() {
    if (!this.widget || this.widget.isDestroyed()) return;
    const o = Number(this.configStore.getGeneral().opacity) || 0.94;
    try {
      // CSS 方式（跨模式可靠）；快照里也带 opacity，渲染层每次刷新会自应用
      this.widget.webContents.send('app:opacity', Math.min(1, Math.max(0.4, o)));
    } catch { /* 窗口未就绪时忽略，快照兜底 */ }
  }

  /** 是否支持 Win11 亚克力毛玻璃（build >= 22621 且非透明窗口） */
  static canAcrylic() {
    try {
      const { release, version } = require('os');
      const rel = release(); // 如 10.0.26200
      const build = Number(rel.split('.')[2]);
      return /10\.0\.\d+/.test(rel) && build >= 22621;
    } catch {
      return false;
    }
  }

  /** 主悬浮窗口 */
  createWidget() {
    if (this.widget && !this.widget.isDestroyed()) {
      this.widget.show();
      this.widget.focus();
      return this.widget;
    }
    const cfg = this.configStore.get();
    const bounds = cfg.windowBounds || {};
    const pinned = !!(cfg.general && cfg.general.alwaysOnTop);
    const acrylic = WindowManager.canAcrylic();
    const win = new BrowserWindow({
      width: 440,
      height: 700,
      x: bounds.x,
      y: bounds.y,
      frame: false,
      transparent: !acrylic,          // 无 Win11 材质时退回透明窗口
      backgroundMaterial: acrylic ? 'acrylic' : 'none',  // acrylic：真模糊 + 点击时通透玻璃感（用户偏好）
      roundedCorners: acrylic,        // Win11 圆角窗口，避免直角外框
      alwaysOnTop: pinned,            // 默认不置顶，点击左上角图钉切换
      skipTaskbar: true,
      resizable: false,
      maximizable: false,
      fullscreenable: false,
      show: false,
      hasShadow: acrylic,             // acrylic 窗口可用系统阴影
      backgroundColor: acrylic ? '#00000000' : '#00000000',
      webPreferences: this.webPrefs(),
    });
    if (pinned) win.setAlwaysOnTop(true, 'screen-saver');
    win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
    win.loadFile(path.join(RENDERER, 'index.html'), { query: { acrylic: acrylic ? '1' : '0' } });

    win.once('ready-to-show', () => {
      this.restoreOnScreen(win, bounds);
      win.show();
    });
    win.on('moved', () => {
      const b = win.getBounds();
      this.configStore.data.windowBounds = { x: b.x, y: b.y };
      this.configStore.save();
    });
    win.on('closed', () => { this.widget = null; });
    win.webContents.on('context-menu', (_e, params) => {
      Menu.buildFromTemplate([
        { label: '设置', click: () => this.openSettings() },
        { label: '显示/隐藏', click: () => this.toggleWidget() },
        { type: 'separator' },
        { label: '退出', click: () => this.quit() },
      ]).popup({ window: win });
    });
    // 外部链接用浏览器打开
    win.webContents.setWindowOpenHandler(({ url }) => {
      shell.openExternal(url);
      return { action: 'deny' };
    });
    this.widget = win;
    return win;
  }

  restoreOnScreen(win, bounds) {
    try {
      if (bounds && bounds.x != null && bounds.y != null) {
        const s = screen.getDisplayNearestPoint({ x: bounds.x, y: bounds.y }).workArea;
        if (bounds.x >= s.x - 100 && bounds.x <= s.x + s.width - 50 && bounds.y >= s.y - 20 && bounds.y <= s.y + s.height - 50) {
          win.setPosition(Math.round(bounds.x), Math.round(bounds.y));
        }
      }
    } catch { /* 位置异常时使用默认 */ }
  }

  toggleWidget() {
    if (!this.widget || this.widget.isDestroyed()) {
      this.createWidget();
      return;
    }
    if (this.widget.isVisible()) {
      this.widget.hide();
    } else {
      this.widget.show();
      this.widget.focus();
    }
  }

  showWidget() {
    if (!this.widget || this.widget.isDestroyed()) this.createWidget();
    else if (!this.widget.isVisible()) this.widget.show();
  }

  /** 钉在前排：切换置顶状态并持久化到配置 */
  setPinned(v) {
    const pinned = !!v;
    if (this.widget && !this.widget.isDestroyed()) {
      if (pinned) this.widget.setAlwaysOnTop(true, 'screen-saver');
      else this.widget.setAlwaysOnTop(false);
    }
    try {
      this.configStore.data.general = this.configStore.data.general || {};
      this.configStore.data.general.alwaysOnTop = pinned;
      this.configStore.save();
    } catch { /* 持久化失败不影响置顶切换 */ }
    return pinned;
  }

  /** 设置窗口 */
  openSettings() {
    if (this.settings && !this.settings.isDestroyed()) {
      this.settings.show();
      this.settings.focus();
      return this.settings;
    }
    const win = new BrowserWindow({
      width: 780,
      height: 680,
      minWidth: 640,
      minHeight: 520,
      frame: false,
      show: false,
      backgroundColor: '#101216',
      webPreferences: this.webPrefs(),
    });
    win.loadFile(path.join(RENDERER, 'settings.html'));
    win.once('ready-to-show', () => win.show());
    win.on('closed', () => { this.settings = null; });
    win.webContents.setWindowOpenHandler(({ url }) => {
      shell.openExternal(url);
      return { action: 'deny' };
    });
    this.settings = win;
    return win;
  }

  closeSettings() {
    if (this.settings && !this.settings.isDestroyed()) this.settings.close();
  }

  quit() {
    this.settings?.destroy();
    this.widget?.destroy();
    require('electron').app.quit();
  }
}

module.exports = { WindowManager };
