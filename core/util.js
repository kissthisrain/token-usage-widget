'use strict';
// 通用工具：日期/数字/金额格式化（主进程与渲染进程共用逻辑）

function pad2(n) {
  return String(n).padStart(2, '0');
}

function dateKey(ts) {
  const d = new Date(ts);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

function todayKey() {
  return dateKey(Date.now());
}

/** 天数（含当天）之前的日期 key */
function daysAgoKey(days) {
  return dateKey(Date.now() - days * 86400000);
}

function parseTs(v) {
  if (v == null) return null;
  if (typeof v === 'number') return v < 1e12 ? v * 1000 : v; // 秒 → 毫秒
  const t = Date.parse(v);
  return isNaN(t) ? null : t;
}

function fmtInt(n) {
  if (n == null || !isFinite(n)) return '—';
  return Math.round(n).toLocaleString('en-US');
}

/** 紧凑数字：1.2B / 450.1M / 12.3K */
function fmtCompact(n) {
  if (n == null || !isFinite(n)) return '—';
  const abs = Math.abs(n);
  if (abs >= 1e9) return (n / 1e9).toFixed(2).replace(/\.?0+$/, '') + 'B';
  if (abs >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, '') + 'M';
  if (abs >= 1e3) return (n / 1e3).toFixed(1).replace(/\.0$/, '') + 'K';
  return String(Math.round(n));
}

const CNY_SYMS = ['CNY', 'cny', 'CN¥', '¥', 'RMB'];

function isCny(currency) {
  return CNY_SYMS.includes(currency);
}

function moneySym(currency) {
  return isCny(currency) ? '¥' : '$';
}

function fmtMoney(n, currency) {
  if (n == null || !isFinite(n)) return '—';
  const sym = moneySym(currency);
  const v = Number(n);
  if (v >= 1e6) return sym + fmtCompact(v);
  if (v === 0) return sym + '0.00';
  return sym + v.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 倒计时文案：6d 15h / 2h 9m / 9m */
function fmtCountdown(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function fmtPct(v) {
  if (v == null || !isFinite(v)) return '—';
  return Math.round(v) + '%';
}

/** 去抖 */
function debounce(fn, ms) {
  let t = null;
  return function (...args) {
    if (t) clearTimeout(t);
    t = setTimeout(() => { t = null; fn(...args); }, ms);
  };
}

module.exports = {
  pad2, dateKey, todayKey, daysAgoKey, parseTs,
  fmtInt, fmtCompact, fmtMoney, fmtCountdown, fmtPct, isCny, debounce,
};
