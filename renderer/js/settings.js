'use strict';
// 设置窗口逻辑

const api = window.TW;
const U = window.U;
const $ = (id) => document.getElementById(id);

let cfg = null;
let selectedId = null;
let detections = null;
let opacityVal = 94; // 透明度当前值（自绘滑块状态）

const TYPE_META = {
  'local-logs': { icon: '📁', label: '本地记录' },
  'openai': { icon: '🔷', label: 'OpenAI 官方' },
  'compat': { icon: '🔀', label: '兼容中转站' },
  'opencode-go-official': { icon: '⚡', label: 'Go 官方额度' },
  'opencode-go-proxy': { icon: '🔌', label: 'Go 本地代理' },
};

const DEFAULTS = {
  'local-logs': {
    name: '本地记录', tool: 'zcode', path: '', recursive: true,
    filePattern: '.*\\.jsonl$', format: 'zcode', modelWhitelist: '',
  },
  'openai': {
    name: 'OpenAI', baseUrl: 'https://api.openai.com', apiKey: '',
    organizationId: '', modelWhitelist: '',
  },
  'compat': {
    name: '兼容中转站', baseUrl: '', apiKey: '', balanceMode: 'auto',
    customBalanceUrl: '', modelWhitelist: '',
  },
  'opencode-go-official': {
    name: 'OpenCode Go 官方额度', workspaceId: '', authCookie: '', modelWhitelist: '',
  },
  'opencode-go-proxy': {
    name: 'OpenCode Go 本地代理', baseUrl: 'https://opencode.ai/zen/go', port: 12345,
    priceMap: {}, modelWhitelist: '',
  },
};

// ================= 数据源列表 =================

function renderSrcList() {
  const box = $('srcItems');
  box.innerHTML = cfg.sources.map((s) => {
    const meta = TYPE_META[s.type] || { icon: '❔', label: s.type };
    return `<div class="src-item ${s.id === selectedId ? 'selected' : ''}" data-id="${U.esc(s.id)}">
      <div class="s-icon">${meta.icon}</div>
      <div class="s-name">
        <div class="t">${U.esc(s.name)}</div>
        <div class="ty">${meta.label}</div>
      </div>
      <div class="s-status ${s.enabled ? 'on' : ''}" title="${s.enabled ? '已启用' : '已停用'}"></div>
    </div>`;
  }).join('');
  box.querySelectorAll('.src-item').forEach((el) => {
    el.addEventListener('click', () => { selectedId = el.dataset.id; renderSrcList(); renderEditor(); });
  });
  if (!selectedId && cfg.sources.length) selectedId = cfg.sources[0].id;
}

// ================= 编辑器 =================

function cur() { return cfg.sources.find((s) => s.id === selectedId) || null; }

function renderEditor() {
  const box = $('srcEdit');
  const s = cur();
  if (!s) {
    box.innerHTML = `<div class="edit-card edit-empty">选择或添加一个数据源</div>`;
    return;
  }
  const enabledBox = `<div class="enabled-box"><span>启用</span>
    <label class="switch"><input type="checkbox" id="e-enabled" ${s.enabled ? 'checked' : ''} /><span></span></label></div>`;
  const common = `
    <div class="edit-head"><h3>${U.esc(s.name)}</h3>${enabledBox}</div>
    <div class="field"><label>名称</label><input id="e-name" value="${U.esc(s.name)}" /></div>
    ${field('e-whitelist', '模型白名单（逗号分隔，留空显示全部）', textarea('e-whitelist', (s.modelWhitelist || []).join(', ')))}
    ${statusHtml()}`;

  let body = '';
  if (s.type === 'local-logs') {
    body = `
      <div class="field"><label>工具类型</label><select id="e-tool">
        ${['zcode', 'codex', 'dsh', 'workbuddy', 'custom'].map((t) => `<option value="${t}" ${s.tool === t ? 'selected' : ''}>${t === 'zcode' ? 'ZCode' : t === 'codex' ? 'Codex' : t === 'dsh' ? 'DSH' : t === 'workbuddy' ? 'WorkBuddy' : '自定义'}</option>`).join('')}
      </select></div>
      <div class="field"><label>记录目录路径</label><input id="e-path" value="${U.esc(s.path || '')}" placeholder="如 C:\\Users\\xxx\\.zcode\\cli\\rollout" /></div>
      <div class="field" id="f-sqlite" ${s.tool === 'zcode' ? '' : 'style="display:none"'}>
        <label>历史数据库路径（ZCode）</label><input id="e-sqlite" value="${U.esc(s.sqliteDb || '')}" placeholder="留空自动检测 ~/.zcode/cli/db/db.sqlite" />
        <div class="hint">实时日志只保留当天，历史用量从 db.sqlite 的 model_usage 表回填（只读，自动按天去重）</div>
      </div>
      <div class="field" id="f-custom" ${s.tool === 'custom' ? '' : 'style="display:none"'}>
        <label>文件匹配（正则）</label><input id="e-filepattern" value="${U.esc(s.filePattern || '.*\\.jsonl$')}" /></div>`;
  } else if (s.type === 'openai') {
    body = `
      <div class="field"><label>API Base URL</label><input id="e-baseurl" value="${U.esc(s.baseUrl || '')}" /></div>
      <div class="field"><label>API Key（需组织管理员权限）</label><input id="e-apikey" type="password" value="${U.esc(s.apiKey || '')}" /></div>
      <div class="field"><label>组织 ID（可选）</label><input id="e-orgid" value="${U.esc(s.organizationId || '')}" /></div>
      <div class="field"><div class="hint">用量接口需要「组织管理员」API Key（platform.openai.com → Organization → API keys），普通 Key 无用量权限</div></div>`;
  } else if (s.type === 'compat') {
    body = `
      <div class="field"><label>API Base URL</label><input id="e-baseurl" value="${U.esc(s.baseUrl || '')}" placeholder="如 https://xxx.com/v1" /></div>
      <div class="field"><label>API Key</label><input id="e-apikey" type="password" value="${U.esc(s.apiKey || '')}" /></div>
      <div class="field"><label>余额接口模式</label><select id="e-balancemode">
        ${[
          ['auto', '自动探测（推荐）'],
          ['openai-billing', 'OpenAI billing 风格 (/v1/dashboard/billing/usage)'],
          ['deepseek-balance', 'DeepSeek 风格 (/v1/user/balance)'],
          ['openrouter-key', 'OpenRouter 风格 (/api/v1/auth/key)'],
          ['one-api', 'one-api / new-api 风格 (/api/user/self)'],
          ['custom', '自定义接口'],
        ].map(([v, t]) => `<option value="${v}" ${s.balanceMode === v ? 'selected' : ''}>${t}</option>`).join('')}
      </select></div>
      <div class="field" id="f-customurl" ${s.balanceMode === 'custom' ? '' : 'style="display:none"'}>
        <label>自定义余额接口 URL</label><input id="e-customurl" value="${U.esc(s.customBalanceUrl || '')}" placeholder="GET 请求，返回 JSON" />
        <div class="hint">返回的 JSON 中需包含：total_usage / balance_infos / usage+limit / quota 等可识别字段</div>
      </div>`;
  } else if (s.type === 'opencode-go-official' || s.type === 'opencode-go') {
    // 官方额度源：只查 opencode.ai 控制台（workspaceId + Cookie）
    body = `
      <div class="field"><label>工作区 ID（wrk_…）</label><input id="e-workspaceid" value="${U.esc(s.workspaceId || '')}" placeholder="控制台 URL 中 /workspace/{id}/go 的 id" />
        <div class="hint">获取：浏览器登录 opencode.ai → 打开用量控制台 → 地址栏复制 wrk_ 开头的 ID</div>
      </div>
      <div class="field"><label>Auth Cookie（登录态，仅本地保存）</label><input id="e-authcookie" type="password" value="${U.esc(s.authCookie || '')}" placeholder="浏览器 Cookie 中 auth 的值" />
        <div class="hint">获取：控制台页面按 F12 → Application/存储 → Cookies → opencode.ai → 复制 auth 的值。<b>Cookie 等同登录态，请勿外传</b>；数据为官方权威（包含所有设备）</div>
      </div>`;
  } else if (s.type === 'opencode-go-proxy') {
    // 本地代理源：记录本机请求消耗（估算）
    body = `
      <div class="field"><label>代理端口</label><input id="e-port" type="number" value="${U.esc(s.port || 12345)}" /></div>
      <div class="field"><label>上游 Base URL</label><input id="e-baseurl" value="${U.esc(s.baseUrl || 'https://opencode.ai/zen/go')}" /></div>
      <div class="field"><label>模型价目表（每 1M token 美元，格式：模型: 输入价/输出价）</label>
        ${textarea('e-price', priceMapToText(s.priceMap), 'deepseek-v4-flash: 0.14/0.28')}
        <div class="hint">留空使用内置官方价目表；未知模型的费用不计入限额计算</div>
      </div>
      <div class="field"><label>代理状态</label>
        <div class="proxy-box"><span id="proxyStatus">…</span>
          <button id="proxyStart">启动代理</button><button id="proxyStop">停止代理</button></div>
        <div class="hint" id="proxyHint"></div>
      </div>`;
  }

  box.innerHTML = `<div class="edit-card">${common}${body}
    <div class="edit-actions">
      <button id="e-test" class="wide">测试连接</button>
      <button id="e-refresh">立即刷新</button>
      <button id="e-delete" class="danger">删除</button>
    </div>
  </div>`;
  bindEditor(s);
}

function field(id, label, inner) {
  return `<div class="field"><label>${label}</label>${inner}</div>`;
}
function textarea(id, v, placeholder) {
  return `<textarea id="${id}" placeholder="${placeholder ? U.esc(placeholder) : ''}">${U.esc(v || '')}</textarea>`;
}
function statusHtml() {
  return `<div class="field"><div class="hint" id="e-status"></div></div>`;
}

// 编辑器的 HTML → 写回 cfg
function syncEditorToCfg(s) {
  const v = (id) => {
    const el = document.getElementById(id);
    return el ? el.value : undefined;
  };
  if (v('e-name') !== undefined) s.name = v('e-name');
  if (v('e-enabled') !== undefined) s.enabled = document.getElementById('e-enabled').checked;
  if (v('e-tool') !== undefined) s.tool = v('e-tool');
  if (v('e-path') !== undefined) s.path = v('e-path');
  if (v('e-sqlite') !== undefined) s.sqliteDb = v('e-sqlite') || undefined;
  if (v('e-filepattern') !== undefined) s.filePattern = v('e-filepattern');
  if (v('e-baseurl') !== undefined) s.baseUrl = v('e-baseurl');
  if (v('e-apikey') !== undefined) s.apiKey = v('e-apikey');
  if (v('e-orgid') !== undefined) s.organizationId = v('e-orgid');
  if (v('e-balancemode') !== undefined) s.balanceMode = v('e-balancemode');
  if (v('e-customurl') !== undefined) s.customBalanceUrl = v('e-customurl');
  if (v('e-port') !== undefined) s.port = Number(v('e-port')) || 12345;
  if (v('e-workspaceid') !== undefined) s.workspaceId = v('e-workspaceid').trim();
  if (v('e-authcookie') !== undefined) s.authCookie = v('e-authcookie').trim();
  if (v('e-price') !== undefined) s.priceMap = parsePriceMap(v('e-price'));
  if (v('e-whitelist') !== undefined) {
    s.modelWhitelist = v('e-whitelist').split(/[,，]/).map((x) => x.trim()).filter(Boolean);
  }
}

function parsePriceMap(text) {
  const map = {};
  for (const line of String(text || '').split('\n')) {
    const l = line.trim();
    if (!l || l.startsWith('#') || l.startsWith('//')) continue;
    if (l.startsWith('{')) {
      try { Object.assign(map, JSON.parse(l)); continue; } catch { /* 忽略坏行 */ }
    }
    const m = l.match(/^([\w.\-]+)\s*[:=]\s*([\d.]+)\s*\/\s*([\d.]+)/);
    if (m) map[m[1]] = { input: +m[2], output: +m[3] };
  }
  return map;
}

function priceMapToText(map) {
  return Object.entries(map || {}).map(([k, v]) => `${k}: ${v.input}/${v.output}`).join('\n');
}

function setStatus(msg, cls) {
  const el = $('e-status');
  if (!el) return;
  el.textContent = msg || '';
  el.className = 'hint' + (cls ? ' ' + cls : '');
}

function bindEditor(s) {
  const enabled = document.getElementById('e-enabled');
  if (enabled) enabled.addEventListener('change', () => { s.enabled = enabled.checked; renderSrcList(); });

  const toolSel = document.getElementById('e-tool');
  if (toolSel) {
    const f = document.getElementById('f-custom');
    const fs = document.getElementById('f-sqlite');
    const showCustom = () => {
      if (f) f.style.display = toolSel.value === 'custom' ? '' : 'none';
      if (fs) fs.style.display = toolSel.value === 'zcode' ? '' : 'none';
    };
    toolSel.addEventListener('change', () => { s.tool = toolSel.value; showCustom(); });
    showCustom();
  }

  const bm = document.getElementById('e-balancemode');
  if (bm) {
    bm.addEventListener('change', () => {
      s.balanceMode = bm.value;
      document.getElementById('f-customurl').style.display = bm.value === 'custom' ? '' : 'none';
    });
  }

  // 通用输入实时写回（文本类）
  document.querySelectorAll('#srcEdit input[type="text"], #srcEdit input[type="password"], #srcEdit input[type="number"], #srcEdit textarea, #srcEdit select:not(#e-tool):not(#e-balancemode)').forEach((el) => {
    el.addEventListener('change', () => { syncEditorToCfg(s); });
  });

  $('e-delete').addEventListener('click', async () => {
    if (!(await api.ask(`删除数据源「${s.name}」？（历史数据保留）`))) return;
    cfg.sources = cfg.sources.filter((x) => x.id !== s.id);
    selectedId = null;
    renderSrcList();
    renderEditor();
    flash('已删除，记得保存');
  });

  $('e-refresh').addEventListener('click', async () => {
    setStatus('正在刷新…');
    await api.refreshNow();
    setStatus('已触发刷新', 'ok');
  });

  $('e-test').addEventListener('click', async () => {
    syncEditorToCfg(s);
    if (s.type === 'local-logs' && !s.path) { setStatus('请先填写记录目录路径', 'err'); return; }
    if ((s.type === 'openai' || s.type === 'compat') && !s.apiKey) { setStatus('请先填写 API Key', 'err'); return; }
    setStatus('正在测试…');
    const r = await api.testSource(JSON.parse(JSON.stringify(s)));
    if (r.ok) setStatus('✓ ' + (r.message || '连接成功'), 'ok');
    else setStatus('✗ ' + (r.error || '连接失败'), 'err');
  });

  // opencode-go 本地代理源：显示代理状态控件
  if (s.type === 'opencode-go-proxy') refreshProxy();
}

async function refreshProxy() {
  const st = await api.proxyStatus();
  const el = $('proxyStatus');
  if (!el) return;
  el.textContent = st.running ? `运行中 · ${st.host}:${st.port}` : '未运行';
  el.style.color = st.running ? 'var(--accent)' : 'var(--text-faint)';
  const hint = $('proxyHint');
  if (hint) hint.textContent = st.hint;
  const start = $('proxyStart'), stop = $('proxyStop');
  if (start) {
    start.disabled = st.running;
    stop.disabled = !st.running;
    start.onclick = async () => { await api.proxyStart(); refreshProxy(); };
    stop.onclick = async () => { await api.proxyStop(); refreshProxy(); };
  }
}

// ================= 添加 / 探测 =================

function addSource(type) {
  const s = {
    id: 'src_' + Date.now().toString(36),
    type,
    enabled: true,
    displayOrder: cfg.sources.length,
    ...JSON.parse(JSON.stringify(DEFAULTS[type])),
  };
  cfg.sources.push(s);
  selectedId = s.id;
  renderSrcList();
  renderEditor();
}

function normPath(p) {
  return String(p || '').replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

async function renderDetections() {
  if (detections == null) detections = await api.rescanTools();
  const box = $('detectResults');
  box.innerHTML = detections.filter((d) => d.exists).map((d) => `
    <div class="det-tool">
      <span class="d-name">${U.esc(d.name)}</span> · ${U.fmtInt(d.recordCount)} 条记录
      <button data-id="${d.id}" data-tool="${d.tool}" data-path="${U.esc(d.path)}">添加</button>
    </div>`).join('');
  box.querySelectorAll('button').forEach((b) => {
    b.addEventListener('click', () => {
      const t = { id: b.dataset.id, tool: b.dataset.tool, path: b.dataset.path };
      if (cfg.sources.some((s) => s.type === 'local-logs' && normPath(s.path) === normPath(t.path))) {
        api.alert('该工具已添加过（路径相同），请勿重复添加以免重复计数');
        return;
      }
      const s = {
        id: 'src_' + t.id + '_' + Date.now().toString(36),
        type: 'local-logs',
        tool: t.tool,
        name: t.name || t.id,
        path: t.path,
        enabled: true,
        displayOrder: cfg.sources.length,
        modelWhitelist: [],
      };
      cfg.sources.push(s);
      selectedId = s.id;
      renderSrcList();
      renderEditor();
      flash('已添加「' + s.name + '」，保存后生效');
    });
  });
}

// ================= 通用页 =================

function loadGeneral() {
  const g = cfg.general;
  $('gInterval').value = g.refreshIntervalMin;
  $('gHotkey').value = g.hotkey || '';
  $('gAutoLaunch').checked = g.autoLaunch;
  $('gShowCost').checked = g.showCost;
  $('gShowQuota').checked = g.showQuota;
  $('gShowCharts').checked = g.showCharts;
  $('gShowModelList').checked = g.showModelList;
  $('gOpacity').tabIndex = 0;
  opacityVal = Math.round((g.opacity || 0.94) * 100);
  updateOpacityFill();
  initOpacitySlider();
}

function updateOpacityFill() {
  const v = opacityVal;
  $('gOpacityFill').style.width = v + '%';
  // 圆点右缘对齐填充末端（减去半颗圆点宽 8.5px），100% 时严丝合缝
  $('gOpacityThumb').style.left = `calc(${v}% - 8.5px)`;
  $('gOpacityVal').textContent = v + '%';
}

/** 自绘滑块交互：拖动 / 点击 / 键盘方向键 */
function initOpacitySlider() {
  const slider = $('gOpacity');
  const clamp = (v) => Math.min(100, Math.max(20, Math.round(v)));
  const setFromX = (clientX) => {
    const r = slider.getBoundingClientRect();
    opacityVal = clamp(((clientX - r.left) / r.width) * 100);
    updateOpacityFill();
  };
  let dragging = false;
  slider.addEventListener('pointerdown', (e) => {
    dragging = true;
    slider.setPointerCapture(e.pointerId);
    setFromX(e.clientX);
  });
  slider.addEventListener('pointermove', (e) => { if (dragging) setFromX(e.clientX); });
  slider.addEventListener('pointerup', () => { dragging = false; });
  slider.addEventListener('pointercancel', () => { dragging = false; });
  slider.addEventListener('keydown', (e) => {
    if (['ArrowLeft', 'ArrowDown'].includes(e.key)) { opacityVal = clamp(opacityVal - 1); updateOpacityFill(); e.preventDefault(); }
    if (['ArrowRight', 'ArrowUp'].includes(e.key)) { opacityVal = clamp(opacityVal + 1); updateOpacityFill(); e.preventDefault(); }
  });
}

function saveGeneral() {
  const g = cfg.general;
  g.refreshIntervalMin = Math.max(1, Number($('gInterval').value) || 10);
  g.hotkey = $('gHotkey').value.trim();
  g.autoLaunch = $('gAutoLaunch').checked;
  g.showCost = $('gShowCost').checked;
  g.showQuota = $('gShowQuota').checked;
  g.showCharts = $('gShowCharts').checked;
  g.showModelList = $('gShowModelList').checked;
  g.opacity = opacityVal / 100;
}

// ================= 保存 / 状态 =================

function flash(msg, isErr) {
  const el = $('saveStatus');
  el.textContent = msg;
  el.className = 'save-status' + (isErr ? ' err' : '');
  clearTimeout(flash._t);
  flash._t = setTimeout(() => { el.textContent = ''; }, 4000);
}

async function save() {
  saveGeneral();
  for (const s of cfg.sources) {
    if (s.id === selectedId) syncEditorToCfg(s);
  }
  const r = await api.saveConfig(cfg);
  if (r.ok) flash('已保存 ✓（小组件已实时刷新）');
  else flash('保存失败：' + (r.error || '未知错误'), true);
}

// ================= 初始化 =================

async function init() {
  cfg = await api.getConfig();
  selectedId = cfg.sources.length ? cfg.sources[0].id : null;
  renderSrcList();
  renderEditor();
  loadGeneral();
  renderDetections();

  // Tabs
  document.querySelectorAll('.tab').forEach((t) => {
    t.addEventListener('click', () => {
      document.querySelectorAll('.tab').forEach((x) => x.classList.remove('active'));
      document.querySelectorAll('.tab-pane').forEach((x) => x.classList.remove('active'));
      t.classList.add('active');
      $('tab-' + t.dataset.tab).classList.add('active');
    });
  });

  // 添加
  $('addType').addEventListener('change', (e) => {
    if (e.target.value) { addSource(e.target.value); e.target.value = ''; }
  });

  // 探测
  $('rescanBtn').addEventListener('click', async () => {
    detections = null;
    await renderDetections();
  });

  // 通用操作
  $('saveBtn').addEventListener('click', save);
  $('refreshNowBtn').addEventListener('click', async () => {
    await api.refreshNow();
    flash('已触发全部数据源刷新');
  });
  $('exportBtn').addEventListener('click', async () => {
    const r = await api.exportData();
    flash(r.ok ? `已导出：${r.path}` : '导出失败', !r.ok);
  });
  $('clearBtn').addEventListener('click', async () => {
    if (!(await api.ask('确定清除全部历史用量数据？此操作不可恢复。'))) return;
    await api.clearData();
    flash('已清除全部数据');
  });

  $('minBtn').addEventListener('click', () => api.winMinimize());
  $('closeBtn').addEventListener('click', () => api.winClose());
}

init();
