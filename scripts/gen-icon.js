'use strict';
// 生成应用图标 resources/icon.png（32x32）与托盘图标 resources/tray.png（16x16）
// 纯 Node 实现 PNG 编码（zlib + 手写 CRC32），无第三方依赖
// 图案：深色圆角方块 + 青色数据条（token 用量主题）

const zlib = require('zlib');
const fs = require('fs');
const path = require('path');

// ---------- PNG 编码 ----------
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, 'ascii');
  const crcBuf = Buffer.alloc(4);
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])));
  return Buffer.concat([len, typeBuf, data, crcBuf]);
}

function encodePng(width, height, rgba) {
  const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;  // bit depth
  ihdr[9] = 6;  // color type RGBA
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0; // filter none
    rgba.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  }
  const idat = zlib.deflateSync(raw, { level: 9 });
  return Buffer.concat([sig, chunk('IHDR', ihdr), chunk('IDAT', idat), chunk('IEND', Buffer.alloc(0))]);
}

// ---------- 图案绘制 ----------
function drawIcon(size) {
  const px = Buffer.alloc(size * size * 4);
  const s = size;
  const r = Math.max(2, Math.round(s * 0.24)); // 圆角半径
  const inRound = (x, y) => {
    const cx = Math.min(Math.max(x, r), s - 1 - r);
    const cy = Math.min(Math.max(y, r), s - 1 - r);
    const dx = x - cx, dy = y - cy;
    return dx * dx + dy * dy <= r * r;
  };
  const set = (x, y, cr, cg, cb, a) => {
    const i = (y * s + x) * 4;
    px[i] = cr; px[i + 1] = cg; px[i + 2] = cb; px[i + 3] = a;
  };
  const lerp = (a, b, t) => Math.round(a + (b - a) * t);

  for (let y = 0; y < s; y++) {
    for (let x = 0; x < s; x++) {
      if (!inRound(x, y)) { set(x, y, 0, 0, 0, 0); continue; }
      // 深色渐变底（上深下浅）
      const t = y / s;
      set(x, y, lerp(24, 40, t), lerp(28, 46, t), lerp(42, 66, t), 255);
    }
  }
  // 三条数据条（青/紫/橙）
  const bars = [
    { h: 0.62, c: [34, 211, 238], w: 0.22 },   // teal
    { h: 0.42, c: [139, 92, 246], w: 0.16 },   // violet
    { h: 0.26, c: [251, 146, 60], w: 0.11 },   // amber
  ];
  const margin = Math.round(s * 0.24);
  const bw = Math.round((s - margin * 2) / 3) - Math.round(s * 0.05);
  const maxH = s - margin * 2;
  bars.forEach((b, i) => {
    const bx = margin + i * ((s - margin * 2) / 3);
    const bh = Math.max(2, Math.round(maxH * b.h));
    const top = s - margin - bh;
    for (let y = top; y < s - margin; y++) {
      for (let x = bx; x < bx + bw; x++) {
        if (x < 0 || y < 0 || x >= s || y >= s) continue;
        const t = (y - top) / bh;
        set(x, y, lerp(b.c[0], 255, t * 0.35), lerp(b.c[1], 255, t * 0.35), lerp(b.c[2], 255, t * 0.35), 255);
      }
    }
  });
  return encodePng(size, size, px);
}

// ---------- 输出 ----------
const outDir = path.join(__dirname, '..', 'resources');
fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(path.join(outDir, 'icon.png'), drawIcon(32));
fs.writeFileSync(path.join(outDir, 'tray.png'), drawIcon(16));
console.log('图标已生成:', path.join(outDir, 'icon.png'), '+ tray.png');
