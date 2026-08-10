'use strict';
// 生成 Tauri 图标源图（1024x1024）供 tauri icon 派生全套图标
// 纯 Node 实现 PNG 编码（zlib + 手写 CRC32），无第三方依赖
// 图案：深色圆角方块 + 青色数据条（token 用量主题），与 gen-icon.js 视觉一致

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

// ---------- 图案绘制（含 2x 缩放，保证大尺寸边缘平滑） ----------
function drawIcon(size) {
  const px = Buffer.alloc(size * size * 4);
  const s = size;
  const r = Math.round(s * 0.2355); // 圆角半径
  const margin = Math.round(s * 0.24);
  const bw = Math.round((s - margin * 2) / 3) - Math.round(s * 0.05);
  const maxH = s - margin * 2;
  const bars = [
    { h: 0.62, c: [34, 211, 238] },  // teal
    { h: 0.42, c: [139, 92, 246] },  // violet
    { h: 0.26, c: [251, 146, 60] },  // amber
  ];
  const lerp = (a, b, t) => Math.round(a + (b - a) * t);
  // 预计算每根柱子的矩形区域（整数放大到 4x/supersample 简化处理：直接用像素归约）
  const inRound = (x, y) => {
    const cx = Math.min(Math.max(x + 0.5, r), s - r);
    const cy = Math.min(Math.max(y + 0.5, r), s - r);
    const dx = (x + 0.5) - cx, dy = (y + 0.5) - cy;
    return dx * dx + dy * dy <= r * r;
  };
  for (let y = 0; y < s; y++) {
    for (let x = 0; x < s; x++) {
      if (!inRound(x, y)) { px[(y * s + x) * 4 + 3] = 0; continue; }
      const t = y / s;
      const i = (y * s + x) * 4;
      px[i] = lerp(24, 40, t); px[i + 1] = lerp(28, 46, t); px[i + 2] = lerp(42, 66, t); px[i + 3] = 255;
    }
  }
  bars.forEach((b, idx) => {
    const bx = margin + idx * ((s - margin * 2) / 3);
    const bh = Math.max(2, Math.round(maxH * b.h));
    const top = s - margin - bh;
    for (let y = top; y < s - margin; y++) {
      for (let x = bx; x < bx + bw; x++) {
        if (x < 0 || y < 0 || x >= s || y >= s) continue;
        const i = (y * s + x) * 4;
        if (px[i + 3] === 0) continue;
        const t = (y - top) / bh;
        px[i] = lerp(b.c[0], 255, t * 0.35);
        px[i + 1] = lerp(b.c[1], 255, t * 0.35);
        px[i + 2] = lerp(b.c[2], 255, t * 0.35);
      }
    }
  });
  return encodePng(size, size, px);
}

// ---------- 输出 ----------
const outDir = path.join(__dirname, '..', 'resources');
fs.mkdirSync(outDir, { recursive: true });
const outPng = path.join(outDir, 'icon-1024.png');
fs.writeFileSync(outPng, drawIcon(1024));
console.log('Tauri 图标源已生成:', outPng);
