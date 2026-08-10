'use strict';
// 打包脚本：把 Electron 运行时 + 应用代码复制为独立应用目录
// 产物: dist/token-usage-widget/token-usage-widget.exe —— 双击即用，无需 Node/npm
// 用法: npm run package

const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const outRoot = path.join(root, 'dist');
const appName = 'token-usage-widget';
const appDir = path.join(outRoot, appName);
const electronDist = path.join(root, 'node_modules', 'electron', 'dist');

// 打进 resources/app 的文件/目录（运行时不依赖 node_modules，echarts/sql.js 已本地化）
const APP_FILES = ['main', 'core', 'renderer', 'lib', 'resources', 'node_modules/sql.js', 'package.json'];

function cpDir(src, dest, filter) {
  fs.mkdirSync(dest, { recursive: true });
  for (const ent of fs.readdirSync(src, { withFileTypes: true })) {
    if (filter && !filter(ent.name)) continue;
    const s = path.join(src, ent.name);
    const d = path.join(dest, ent.name);
    if (ent.isDirectory()) cpDir(s, d, filter);
    else fs.copyFileSync(s, d);
  }
}

function step(msg) {
  console.log(msg);
}

// 1. Electron 运行时（resources 单独处理）
step('1/3 复制 Electron 运行时…');
fs.rmSync(appDir, { recursive: true, force: true });
cpDir(electronDist, appDir, (n) => n !== 'resources');
cpDir(path.join(electronDist, 'resources'), path.join(appDir, 'resources'), (n) => n !== 'default_app.asar' && !n.startsWith('default_app'));

// 2. 应用代码 → resources/app
step('2/3 打包应用代码…');
const resApp = path.join(appDir, 'resources', 'app');
for (const f of APP_FILES) {
  const s = path.join(root, f);
  if (!fs.existsSync(s)) continue;
  if (fs.statSync(s).isDirectory()) cpDir(s, path.join(resApp, f));
  else fs.copyFileSync(s, path.join(resApp, f));
}

// 3. 重命名主程序
step('3/3 生成可执行文件…');
fs.renameSync(path.join(appDir, 'electron.exe'), path.join(appDir, appName + '.exe'));

console.log('✔ 打包完成:', path.join(appDir, appName + '.exe'));
console.log('   双击该 exe 即可运行（托盘图标驻留，Ctrl+Alt+U 显示/隐藏）');
