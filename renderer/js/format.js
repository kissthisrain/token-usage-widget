'use strict';
// 渲染进程格式化工具

function pad2(n) { return String(n).padStart(2, '0'); }

function fmtInt(n) {
  if (n == null || !isFinite(n)) return '—';
  return Math.round(n).toLocaleString('en-US');
}

function fmtCompact(n) {
  if (n == null || !isFinite(n)) return '—';
  const abs = Math.abs(n);
  if (abs >= 1e9) return (n / 1e9).toFixed(2).replace(/\.?0+$/, '') + 'B';
  if (abs >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, '') + 'M';
  if (abs >= 1e3) return (n / 1e3).toFixed(1).replace(/\.0$/, '') + 'K';
  return String(Math.round(n));
}

const CNY_SYMS = ['CNY', 'cny', 'CN¥', '¥', 'RMB'];
function isCny(currency) { return CNY_SYMS.includes(currency); }
function moneySym(currency) { return isCny(currency) ? '¥' : '$'; }

function fmtMoney(n, currency) {
  if (n == null || !isFinite(n)) return '—';
  const sym = moneySym(currency);
  const v = Number(n);
  if (v >= 1e6) return sym + fmtCompact(v);
  if (v === 0) return sym + '0.00';
  return sym + v.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 积分符号（非货币单位，如 WorkBuddy 的 credit）：不用 $ 等货币符号，避免被误读成金额。
 *  实心菱形 ⬥（U+2B25）。末尾 U+FE0E 为「文本呈现」变体选择符，强制单色
 *  （否则部分环境会回退成彩色 emoji 字形）。
 *  悬浮到数值上会有「消耗积分」提示解释其含义。改这里即可全局换符号。 */
const CREDITS_SYM = '\u2B25\uFE0E';

/** 积分（非货币，如 WorkBuddy 的 credit）：符号 + 数值，与 $10.98 同风格（无空格） */
function fmtCredits(n) {
  if (n == null || !isFinite(n)) return '—';
  const v = Number(n);
  if (v >= 1e6) return CREDITS_SYM + fmtCompact(v);
  return CREDITS_SYM + v.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

function fmtCountdown(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function fmtDateShort(key) { // 2026-08-07 → 8/7
  const [y, m, d] = key.split('-');
  return `${+m}/${+d}`;
}

/** 数字滚动动画 */
function animateNumber(el, to, { duration = 900, format = fmtInt } = {}) {
  const from = Number(el.dataset.raw || 0) || 0;
  if (to === from) { el.textContent = format(to); return; }
  el.dataset.raw = to;
  const start = performance.now();
  const ease = (t) => 1 - Math.pow(1 - t, 3);
  const step = (now) => {
    const t = Math.min(1, (now - start) / duration);
    const v = from + (to - from) * ease(t);
    el.textContent = format(v);
    if (t < 1) requestAnimationFrame(step);
    else el.textContent = format(to);
  };
  requestAnimationFrame(step);
}

/** 模型颜色（按名称哈希分配） */
const MODEL_COLORS = ['#2dd4bf', '#8b5cf6', '#fb923c', '#38bdf8', '#f472b6', '#a3e635', '#fbbf24', '#60a5fa'];
function modelColor(name) {
  let h = 0;
  const s = String(name || '');
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return MODEL_COLORS[h % MODEL_COLORS.length];
}

function esc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

window.U = { pad2, fmtInt, fmtCompact, fmtMoney, fmtCredits, fmtCountdown, fmtDateShort, animateNumber, modelColor, esc, isCny, moneySym };
