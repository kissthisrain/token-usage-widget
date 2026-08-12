'use strict';
// 悬浮小组件主逻辑

const api = window.TW;
const U = window.U;
const $ = (id) => document.getElementById(id);

const state = {
  range: 'month',
  sourceId: 'all',
  snapshot: null,
  config: null,
  trendChart: null,
  heatmapChart: null,
  pendingRefresh: false,
  lastHeight: 0,
  heightLocked: false,   // 窗口高度锁定（启动按 MONTH 定一次，之后不变）
};

/** 应用卡片透明度（用户设置值；mica 静态材质不随焦点变化，无需补偿） */
function applyCardOpacity() {
  const g = (state.snapshot && state.snapshot.general) || {};
  const base = g.opacity != null ? g.opacity : 0.94;
  document.querySelector('.card').style.opacity = String(Math.min(1, Math.max(0.1, base)));
}

/** 窗口高度：启动时按默认视图（MONTH）内容定一次，之后锁定不变。
 *  切换 DAY/MONTH/TOTAL 时高度不跳动，内容超出用内部滚动（默认展示顶部） */
function autoResize() {
  if (state.heightLocked) return;
  const card = document.querySelector('.card');
  const scroll = $('scroll');
  if (!card || !scroll) return;
  const cRect = card.getBoundingClientRect();
  const sRect = scroll.getBoundingClientRect();
  // 非滚动区域（头部 + 底部导航 + 卡片边距）
  const extra = (sRect.top - cRect.top) + (cRect.bottom - sRect.bottom) + 10;
  const h = Math.ceil(scroll.scrollHeight + extra);
  state.heightLocked = true;         // 锁定：只定一次
  state.lastHeight = h;
  api.resizeWindow(h);
}

// ================= 数据获取 =================

async function refresh() {
  if (state.pendingRefresh) return;
  state.pendingRefresh = true;
  try {
    const snap = await api.getSnapshot({ range: state.range, sourceId: state.sourceId });
    state.snapshot = snap;
    render();
  } catch (e) {
    console.error('快照获取失败', e);
  } finally {
    state.pendingRefresh = false;
  }
}

// ================= 渲染 =================

function render() {
  const s = state.snapshot;
  if (!s) return;
  // 精简界面：只更新对应视图，跳过完整界面渲染
  if (view === 'mini') { renderMini(s); return; }

  // 通用显示设置（设置窗口开关即时生效）
  const g = s.general || {};
  state.g = g;
  applyCardOpacity();
  document.querySelector('.overview').classList.toggle('no-cost', g.showCost === false);
  $('quotaSection').classList.toggle('hidden', g.showQuota === false);
  $('modelsSection').classList.toggle('hidden', g.showModelList === false);
  $('trendSection').classList.toggle('hidden', g.showCharts === false);
  $('activitySection').classList.toggle('hidden', g.showCharts === false);

  // 总览
  U.animateNumber($('totalTokens'), s.totals.tokens, { format: U.fmtInt });
  const costText = s.totals.costPartial ? '~' : '';
  U.animateNumber($('totalCost'), s.totals.cost, {
    format: (v) => (v > 0 || s.totals.cost > 0 ? costText + U.fmtMoney(v, 'USD') : '—'),
  });

  // 活跃天数
  const ad = $('activeDays');
  if (+ad.dataset.v !== s.activeDays) {
    ad.dataset.v = s.activeDays;
    U.animateNumber(ad, s.activeDays, { format: (v) => Math.round(v) });
  }

  renderQuota(s);
  renderModels(s);
  renderCharts(s);
  renderSourceFilter(s);

  // 实时指示
  const hasLive = s.sourcesMeta.some((x) => x.enabled && x.type === 'local-logs');
  $('liveDot').classList.toggle('on', hasLive);

  // 空数据提示
  const hasAny = s.totals.tokens > 0 || s.models.length > 0;
  toggleEmpty(!hasAny && s.sourcesMeta.some((x) => x.enabled));

  // 窗口高度自适应（渲染完成后测量）
  setTimeout(autoResize, 30);
}

function toggleEmpty(show) {
  const exists = $('emptyState');
  if (!show) { if (exists) exists.remove(); return; }
  if (exists) return;
  const el = document.createElement('div');
  el.id = 'emptyState';
  el.className = 'empty';
  el.innerHTML = '<b>暂无用量数据</b><br/>本地记录源在产生新的会话后会自动显示，<br/>API 类数据源请检查配置或点击右下角 ⚙ 设置';
  $('scroll').prepend(el);
}

// ---- 额度（按源分组卡片 + 多周期进度） ----

function renderQuota(s) {
  const wrap = $('quotaSection');
  const list = $('quotaList');
  const quota = (s.quota || []).filter((q) => q.items && q.items.length || (q.ok === false && q.error));
  wrap.classList.toggle('hidden', !quota.length || (state.g && state.g.showQuota === false));
  if (!quota.length) return;

  const nameById = Object.fromEntries(s.sourcesMeta.map((x) => [x.id, x.name]));
  list.innerHTML = quota.map((q) => {
    const srcName = nameById[q.sourceId] || q.sourceId || '';
    const errLine = (q.ok === false && q.error) ? `<div class="quota-err">${U.esc(srcName)}：${U.esc(q.error)}</div>` : '';
    const modeTag = q.mode === 'opencode-go-limits' ? 'Go 估算'
      : q.mode === 'opencode-go-official' ? 'Go 官方'
      : q.mode === 'local-rate-limits' ? '周期额度'
      : q.mode === 'openrouter-key' ? 'Key 额度'
      : q.mode === 'billing' ? '余额'
      : (q.mode || '');
    // 限额信息收敛到卡片头部 tooltip，避免逐列重复
    const lims = (q.items || []).filter((it) => it.limit != null)
      .map((it) => `${it.label} ${U.fmtMoney(it.limit, it.currency)}`);
    const headTitle = lims.length ? `限额：${lims.join(' / ')}` : '';
    const items = q.items || [];
    const cols = items.map((it) => {
      const pct = it.usedPct != null ? Math.round(it.usedPct) : null;
      const barCls = pct == null ? '' : pct >= 95 ? ' red' : pct >= 70 ? ' amber' : pct >= 40 ? ' violet' : '';
      // 顶部右侧：已用百分比；无百分比的余额类源回退显示金额
      const pctText = pct != null ? `${pct}%` : (it.remaining != null ? U.fmtMoney(it.remaining, it.currency) : '');
      // 金额信息放 tooltip，行内不展示
      const tip = it.limit != null
        ? `限额 ${U.fmtMoney(it.limit, it.currency)}${it.spent != null ? `，已用 ${U.fmtMoney(it.spent, it.currency)}` : ''}`
        : '';
      return `<div class="quota-col"${tip ? ` title="${U.esc(tip)}"` : ''}>
        <div class="qc-top"><span class="qc-label">${U.esc(it.label || '')}</span><span class="qc-pct">${U.esc(pctText)}</span></div>
        <div class="qc-bar${barCls}"><i style="width:${pct == null ? 0 : pct}%"></i></div>
        <div class="qc-reset">${renderReset(it)}</div>
      </div>`;
    }).join('');
    return `<div class="quota-card"${headTitle ? ` title="${U.esc(headTitle)}"` : ''}>
      <div class="quota-head"><span class="quota-name">${U.esc(srcName)}</span>${modeTag ? `<span class="src-tag">${U.esc(modeTag)}</span>` : ''}</div>
      ${errLine}
      <div class="quota-cols" style="grid-template-columns:repeat(${Math.max(1, items.length)},1fr)">${cols}</div>
    </div>`;
  }).join('');
}

function renderReset(it) {
  if (it.resetsAt == null) {
    const extra = it.windowMinutes != null ? `窗口 ${Math.round(it.windowMinutes / 60)}h`
      : (it.credits && it.credits.balance != null ? `余额 ${U.fmtMoney(it.credits.balance, 'USD')}` : '');
    return extra ? U.esc(extra) : '';
  }
  const left = it.resetsAt - Date.now();
  if (left > 0) {
    const cls = left < 6 * 3600000 ? ' warn' : '';
    return `重置于 <span class="cd${cls}" data-ts="${it.resetsAt}">${U.fmtCountdown(left)}</span>`;
  }
  return `<span class="expired">已过期</span>`;
}

// 每秒刷新倒计时
function startClock() {
  setInterval(() => {
    document.querySelectorAll('.cd').forEach((el) => {
      const left = +el.dataset.ts - Date.now();
      if (left > 0) el.textContent = U.fmtCountdown(left);
      else el.textContent = '已重置';
    });
  }, 1000);
}

/** 提供商形状图标（不同服务商用不同字形区分） */
const GLYPHS = ['◎', '✶', '◆', '▲', '●', '◈', '✦', '■', '❖', '✧'];
function providerGlyph(name) {
  const s = String(name || '').toLowerCase();
  if (s.includes('zcode')) return '◎';
  if (s.includes('codex')) return '✶';
  if (s.includes('openai')) return '▲';
  if (s.includes('deepseek')) return '◆';
  if (s.includes('claude') || s.includes('anthropic')) return '✦';
  if (s.includes('glm') || s.includes('zhipu')) return '◈';
  if (s.includes('gemini') || s.includes('google')) return '●';
  if (s.includes('go')) return '❖';
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return GLYPHS[h % GLYPHS.length];
}

// ---- 模型明细（占比 = 占总量百分比） ----

function renderModels(s) {
  const wrap = $('modelsSection');
  const list = $('modelList');
  wrap.classList.toggle('hidden', !s.models.length || (state.g && state.g.showModelList === false));
  if (!s.models.length) return;
  const total = s.models.reduce((a, m) => a + m.tokens, 0) || 1;
  list.innerHTML = s.models.slice(0, 30).map((m) => {
    const color = U.modelColor(m.model);
    const glyph = providerGlyph(m.sourceName || m.model);
    const share = Math.round((m.tokens / total) * 100);
    const pct = Math.max(2, share); // 最小可见条
    const cost = m.cost > 0 ? `<span class="cost">${U.fmtMoney(m.cost, 'USD')}</span>` : '';
    const zeroCls = share === 0 ? ' zero' : '';
    return `<div class="model-row${zeroCls}">
      <div class="model-top">
        <span class="model-name"><span class="model-ico" style="color:${color}">${glyph}</span>${U.esc(m.model)}<span class="src-tag">${U.esc(m.sourceName || '')}</span></span>
        <span class="model-nums">${U.fmtCompact(m.tokens)}${cost}${share > 0 ? `<span class="share">${share}%</span>` : ''}</span>
      </div>
      <div class="bar"><i style="width:${pct}%"></i></div>
    </div>`;
  }).join('');
}

// ---- 图表 ----

function renderCharts(s) {
  const show = !(state.g && state.g.showCharts === false);
  // DAY 视图只有一天数据，趋势图无意义 → 隐藏；单点数据同样隐藏
  const hasTrend = s.trend.length > 1 && state.range !== 'day';
  $('trendSection').classList.toggle('hidden', !hasTrend || !show);
  if (hasTrend && show) renderTrend(s.trend);
  $('activitySection').classList.toggle('hidden', !s.heatmap.length || !show);
  if (s.heatmap.length && show) renderHeatmap(s.heatmap);
}

function renderTrend(trend) {
  const el = $('trendChart');
  const doRender = () => {
    const chart = state.trendChart || (state.trendChart = echarts.init(el));
    // 容器刚从隐藏恢复或跨屏 DPI 变化后，初始化读到的尺寸可能未就绪：
    // 强制按当前实际尺寸重算，再等布局稳定后校准一次（避免图表被横向挤压）
    chart.resize();
    setTimeout(() => chart.resize(), 80);
  const dates = trend.map((t) => t.date);
  const tokens = trend.map((t) => t.tokens);
  const len = dates.length;
  // 峰值位于右缘（末三位）时，标注文字向左偏移，避免溢出画布
  const maxVal = Math.max(...tokens);
  const maxIdx = tokens.indexOf(maxVal);
  const peakAtEdge = maxIdx >= len - 3;
  chart.setOption({
    grid: { left: 0, right: 4, top: 24, bottom: 20 },   // 绘图区撑满卡片宽度；底部留 20px 容纳 X 轴日期标签
    xAxis: {
      type: 'category',
      data: dates,
      boundaryGap: false,
      axisLine: { lineStyle: { color: 'rgba(255,255,255,0.14)' } },
      axisTick: { show: false },
      axisLabel: { color: 'rgba(255,255,255,0.5)', fontSize: 9, formatter: U.fmtDateShort, interval: Math.max(0, Math.ceil(len / 6) - 1) },
    },
    yAxis: {
      type: 'value',
      // 标签置于绘图区内部左缘（左对齐），不占外部空间 → 绘图区全宽对齐
      axisLabel: { color: 'rgba(255,255,255,0.45)', fontSize: 8.5, align: 'left', margin: 4, formatter: (v) => U.fmtCompact(v) },
      splitLine: { lineStyle: { color: 'rgba(255,255,255,0.07)' } },
      axisLine: { show: false },
    },
    tooltip: {
      trigger: 'axis',
      backgroundColor: 'rgba(18,20,30,0.95)',
      borderColor: 'rgba(255,255,255,0.15)',
      textStyle: { color: '#f4f6fb', fontSize: 11 },
      formatter: (ps) => {
        const p = ps[0];
        return `${p.axisValue}<br/><span style="color:#2dd4bf">●</span> ${U.fmtInt(p.value)} tokens`;
      },
    },
    series: [
      // 底层：宽线条低透明度 → 发光/光晕
      {
        type: 'line',
        data: tokens,
        smooth: 0.4,
        symbol: 'none',
        z: 1,
        lineStyle: { color: 'rgba(45, 212, 191, 0.28)', width: 7 },
      },
      // 上层：细亮线 + 同色系渐变填充
      {
        type: 'line',
        data: tokens,
        smooth: 0.4,
        symbol: 'none',
        z: 2,
        lineStyle: { color: '#2dd4bf', width: 2.2, shadowColor: 'rgba(45,212,191,0.8)', shadowBlur: 10 },
        areaStyle: {
          color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [
            { offset: 0, color: 'rgba(45,212,191,0.38)' },
            { offset: 1, color: 'rgba(45,212,191,0.02)' },
          ]),
        },
        markPoint: {
          symbol: 'circle',
          symbolSize: 7,
          itemStyle: { color: '#2dd4bf', borderColor: '#fff', borderWidth: 1.5, shadowColor: 'rgba(45,212,191,0.9)', shadowBlur: 8 },
          label: {
            formatter: (p) => '峰值 ' + U.fmtCompact(p.value),
            color: '#fff', fontSize: 9,
            // 峰值在右缘时左移标注（约一条标签宽度），保证完整显示且不破坏全宽对齐
            position: peakAtEdge ? [-52, -13] : 'top',
          },
          data: [{ type: 'max', name: '峰值' }],
        },
      },
    ],
  });
    window.__trendChart = chart; // 调试转储用
  };
  if (state.trendChart) {
    doRender();
  } else {
    // 容器刚从隐藏恢复/跨屏后，浏览器布局可能未完成：
    // 双 rAF 后再 init，确保拿到正确的容器宽度（避免图表被横向挤压）
    requestAnimationFrame(() => requestAnimationFrame(doRender));
  }
}

function renderHeatmap(heatmap) {
  const el = $('heatmapChart');
  const chart = state.heatmapChart || (state.heatmapChart = echarts.init(el));
  // 保留全部数据（含 0 消耗天），让 visualMap 统一着色：0 → 浅灰，>0 → 青绿分档
  const data = heatmap.map((d) => [d.date, d.tokens]);
  // 分位数分档着色：低消耗天也能落在可见色阶上（避免线性映射把极小值压成无色）
  const vals = data.map(([, v]) => v).filter((v) => v > 0).sort((a, b) => a - b);
  const q = (p) => vals[Math.min(vals.length - 1, Math.floor(vals.length * p))];
  const boundaries = vals.length
    ? [...new Set([q(0.25), q(0.5), q(0.75), q(0.9), vals[vals.length - 1]])]
    : [];
  const PALETTE = [0.42, 0.55, 0.68, 0.8, 0.9, 0.96].map((a) => `rgba(45,212,191,${a})`);
  const GRAY = 'rgba(180,190,200,0.28)'; // 浅灰：统一走 visualMap 着色，与有 token 格子同路径
  const pieces = [{ min: 0, max: 0, color: GRAY }]; // 0 消耗天 → 浅灰
  let lo = 0;
  for (let i = 0; i < boundaries.length; i++) {
    // 档位少时也均匀取色：最低档 0.42 可见，最高档接近满色
    const ci = Math.round((i / Math.max(1, boundaries.length - 1)) * (PALETTE.length - 1));
    pieces.push({ gt: lo, lte: boundaries[i], color: PALETTE[Math.min(ci, PALETTE.length - 1)] });
    lo = boundaries[i];
  }
  const first = heatmap[0].date;
  chart.setOption({
    tooltip: {
      backgroundColor: 'rgba(18,20,30,0.95)',
      borderColor: 'rgba(255,255,255,0.15)',
      textStyle: { color: '#f4f6fb', fontSize: 11 },
      formatter: (p) => `${p.value[0]}<br/>${U.fmtInt(p.value[1])} tokens`,
    },
    visualMap: {
      show: false,
      type: 'piecewise',
      pieces,
    },
    calendar: {
      left: 2,
      right: 2,
      top: 2,
      bottom: 20,                   // 底部月份标签空间（要容纳完整 10px 文字，否则下半截被裁）
      cellSize: ['auto', 11],       // 紧凑 GitHub 风格：11px 行高（避免扁），宽度自动撑满容器
      range: [first, heatmap[heatmap.length - 1].date],
      splitLine: { show: false },
      itemStyle: { color: 'transparent', borderWidth: 4, borderColor: 'rgba(0,0,0,0)' },
      dayLabel: { show: false },
      monthLabel: {
        show: true,
        position: 'end',            // 底部月份标签
        color: 'rgba(255,255,255,0.85)',   // 提亮：确保可见
        fontSize: 10,
        // nameMap 数组直接指定 1月~12月（formatter 的 p.month 是字符串，勿做算术）
        nameMap: ['1月', '2月', '3月', '4月', '5月', '6月', '7月', '8月', '9月', '10月', '11月', '12月'],
      },
      yearLabel: { show: false },
    },
    series: [{
      type: 'scatter',
      coordinateSystem: 'calendar',
      symbol: 'roundRect',
      symbolSize: () => [11, 11], // 略小于 cellSize 11，留出间隙；圆角由 symbol 决定
      itemStyle: { color: '#2dd4bf' }, // 兜底色（visualMap 未命中时使用）
      encode: { tooltip: 1 },
      data,
    }],
  });
  window.__heatmapChart = chart; // 调试转储用
}

// ---- 数据源下拉 ----

function renderSourceFilter(s) {
  const sel = $('sourceFilter');
  const enabled = s.sourcesMeta.filter((x) => x.enabled);
  const cur = state.sourceId;
  sel.innerHTML =
    `<option value="all">全部数据源 (${enabled.length})</option>` +
    enabled.map((x) => `<option value="${U.esc(x.id)}">${U.esc(x.name)}</option>`).join('');
  if (enabled.some((x) => x.id === cur)) sel.value = cur;
  else { sel.value = 'all'; state.sourceId = 'all'; }
}

// ================= 引导页 =================

async function showOnboarding() {
  $('main').classList.add('hidden');
  $('onboarding').classList.remove('hidden');
  await scanTools();
}

async function scanTools() {
  const tools = await api.rescanTools();
  const box = $('ob-tools');
  box.innerHTML = tools.map((t) => {
    if (!t.exists) {
      return `<div class="ob-tool notfound">
        <input type="checkbox" class="cb" disabled />
        <div class="info">
          <div class="t-name">${U.esc(t.name)}</div>
          <div class="t-desc">未检测到（${U.esc(t.desc || '')}）</div>
        </div>
      </div>`;
    }
    const models = (t.models || []).slice(0, 3).join(' · ');
    const range = t.dateFrom ? `${t.dateFrom} ~ ${t.dateTo || '今'}` : '';
    return `<div class="ob-tool">
      <input type="checkbox" class="cb" data-id="${t.id}" data-tool="${t.tool}" data-path="${U.esc(t.path)}" checked />
      <div class="info">
        <div class="t-name">${U.esc(t.name)}${t.live ? '<span class="live">实时</span>' : ''}</div>
        <div class="t-desc">${U.esc(t.desc || '')}</div>
        <div class="t-stat">${U.fmtInt(t.recordCount)} 条记录 · ${U.fmtInt(t.fileCount)} 文件${range ? ' · ' + range : ''}${models ? ' · ' + U.esc(models) : ''}</div>
      </div>
    </div>`;
  }).join('');
  window.__tools = tools;
}

async function enableSelectedTools() {
  const checked = [...document.querySelectorAll('#ob-tools .cb:checked')].map((cb) => ({
    id: cb.dataset.id, tool: cb.dataset.tool, path: cb.dataset.path,
  }));
  if (!checked.length) return;
  const cfg = await api.getConfig();
  for (const t of checked) {
    if (cfg.sources.some((s) => s.type === 'local-logs' && s.tool === t.tool && s.path === t.path)) continue;
    cfg.sources.push({
      id: 'src_' + t.id + '_' + Date.now().toString(36),
      type: 'local-logs',
      tool: t.tool,
      name: t.id === 'zcode' ? 'ZCode' : t.id === 'codex' ? 'Codex' : t.id,
      path: t.path,
      enabled: true,
      displayOrder: cfg.sources.length,
      modelWhitelist: [],
    });
  }
  await api.saveConfig(cfg);
  await refresh();
  $('onboarding').classList.add('hidden');
  $('main').classList.remove('hidden');
}

// ================= 精简视图（mini 精简 / main 主界面） =================

let view = 'main';          // 'mini' | 'main'

/** 从快照取 opencode-go 某窗口（5h/week/month）的剩余% */
function quotaItemPct(snap, key) {
  const q = ((snap && snap.quota) || []).find((x) => /opencode-go/.test(x.mode || ''));
  const it = q && (q.items || []).find((i) => i.key === key);
  if (!it || it.usedPct == null) return null;
  return Math.max(0, Math.min(100, 100 - it.usedPct));
}

/** 精简界面（图二：5h 大数字+环形进度，下方周/月剩余） */
function renderMini(snap) {
  const p5 = quotaItemPct(snap, '5h');
  const pw = quotaItemPct(snap, 'week');
  const pm = quotaItemPct(snap, 'month');
  const ring = document.querySelector('.mini-ring');
  const val = document.querySelector('.mini-ring-val');
  const badge = document.querySelector('.mini-badge');
  const rows = document.querySelectorAll('.mini-row .mr-val');
  if (p5 == null) {
    val.textContent = '--';
    ring.style.background = 'conic-gradient(rgba(255,255,255,0.1) 0% 100%)';
    ring.style.boxShadow = '';
    val.style.textShadow = '';
    badge.textContent = '⚠';
    badge.classList.add('err');
  } else {
    // 柔和主视觉：中等亮度进度色 + 收敛光晕；数字淡辉光，避免刺眼
    const color = p5 >= 60 ? '#34d399' : p5 >= 30 ? '#fbbf24' : '#f87171';
    ring.style.background = `conic-gradient(${color} 0% ${p5}%, rgba(255,255,255,0.15) ${p5}% 100%)`;
    ring.style.boxShadow = `0 0 16px ${color}66, 0 0 40px ${color}2e`;
    val.style.textShadow = `0 0 8px ${color}4d, 0 1px 6px rgba(0,0,0,0.7)`;
    val.textContent = Math.round(p5) + '%';
    badge.textContent = '✓';
    badge.classList.remove('err');
  }
  if (rows[0]) rows[0].textContent = pw == null ? '--' : Math.round(pw) + '%';
  if (rows[1]) rows[1].textContent = pm == null ? '--' : Math.round(pm) + '%';
}

/** app:dock 事件 → 切换视图 */
function setView(d) {
  view = d.view || 'main';
  $('miniView').classList.toggle('hidden', view !== 'mini');
  $('main').classList.toggle('hidden', view !== 'main');
  if (view === 'mini') renderMini(state.snapshot);
  if (view === 'main') {
    // 从精简切回主界面：解锁高度锁定并按内容重新自适应
    // （否则窗口高度残留精简界面的小尺寸）
    state.heightLocked = false;
    setTimeout(autoResize, 30);
  }
}

// ================= 初始化 =================

async function init() {
  // 禁用 WebView2 浏览器默认右键菜单（刷新/另存为/打印等）；
  // miniView 自身的右键切回处理不受影响（其 handler 先执行）
  document.addEventListener('contextmenu', (e) => e.preventDefault());

  // 毛玻璃模式（acrylic）时卡片更透明，让背景模糊透出来
  if (new URLSearchParams(location.search).get('acrylic') === '1') {
    document.body.classList.add('acrylic');
  }
  api.onOpacity((v) => {
    if (state.snapshot) state.snapshot.general.opacity = v;
    applyCardOpacity();
  });

  // 精简/主视图切换：Rust 端广播 app:dock {docked:false, view}
  api.onDock((d) => setView(d));
  const miniView = $('miniView');
  // 精简界面右键 → 切换为主界面（居中）
  miniView.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    api.showMain();
  });

  state.config = await api.getConfig();
  const enabledCount = state.config.sources.filter((s) => s.enabled).length;
  if (!enabledCount) {
    await showOnboarding();
  } else {
    $('main').classList.remove('hidden');
    await refresh();
  }
  startClock();
  // 滚动条位置更新
  $('scroll').addEventListener('scroll', () => {});

  // 事件
  document.querySelectorAll('#pills .pill').forEach((p) => {
    p.addEventListener('click', () => {
      document.querySelectorAll('#pills .pill').forEach((x) => x.classList.remove('active'));
      p.classList.add('active');
      state.range = p.dataset.range;
      // 切换范围回到顶部展示
      const sc = $('scroll');
      if (sc) sc.scrollTop = 0;
      refresh();
    });
  });
  $('sourceFilter').addEventListener('change', (e) => {
    state.sourceId = e.target.value;
    refresh();
  });
  $('settingsBtn').addEventListener('click', () => api.openSettings());
  // 左上角图钉：切换置顶（默认不置顶）
  const pinBtn = $('pinBtn');
  if (pinBtn) {
    let pinned = false;
    const applyPin = () => {
      pinBtn.classList.toggle('on', pinned);
      pinBtn.title = pinned ? '取消钉在前排' : '钉在前排（置顶）';
      pinBtn.setAttribute('aria-pressed', String(pinned));
    };
    api.getPinned().then((v) => { pinned = !!v; applyPin(); }).catch(() => {});
    pinBtn.addEventListener('click', async () => {
      const next = !pinned;
      try { pinned = !!(await api.setPinned(next)); } catch { /* 切换失败保持原状 */ }
      applyPin();
    });
  }
  // 模块标题栏操作图标：立即刷新
  document.querySelectorAll('.st-ico[data-refresh]').forEach((b) => {
    b.addEventListener('click', () => api.refreshNow());
  });
  $('ob-settings').addEventListener('click', () => api.openSettings());
  $('ob-refresh').addEventListener('click', scanTools);
  $('ob-enable').addEventListener('click', enableSelectedTools);

  // 实时更新（去抖）
  let t = null;
  api.onUpdate(() => {
    if (t) clearTimeout(t);
    t = setTimeout(() => {
      if (!$('onboarding').classList.contains('hidden')) return;
      refresh();
    }, 350);
  });

  // 每分钟兜底刷新 + 窗口尺寸变化时图表自适应
  setInterval(() => { if (!$('onboarding').classList.contains('hidden')) return; refresh(); }, 60000);
  // 跨屏拖动/DPI 变化时 resize 会连续触发：防抖 + 延迟到尺寸稳定后再校准图表
  // 注意：不要在这里解锁 autoResize——setSize 触发的 resize 会形成“重算→改尺寸→再 resize”死循环
  let resizeT = null;
  window.addEventListener('resize', () => {
    clearTimeout(resizeT);
    resizeT = setTimeout(() => {
      if (state.trendChart) state.trendChart.resize();
      if (state.heatmapChart) state.heatmapChart.resize();
    }, 50);
  });
}

init();
