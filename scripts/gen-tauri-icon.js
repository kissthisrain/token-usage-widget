'use strict';
// 生成应用图标全套源图（纯 Node 实现 PNG 编码：zlib + 手写 CRC32，无第三方依赖）
// 输出：
//   resources/icon-1024.png  主图标源（1024x1024，tauri icon 由此派生全套）
//   resources/icon.png       托盘图标（32x32，src-tauri/src/lib.rs include_bytes 引用）
//   resources/tray.png       托盘图标（16x16，兼容旧资源）
//   resources/logo-preview.png 预览图（512x512，方便直接打开查看）
// 图案：深色玻璃圆角块 + 环形配额进度（青→绿主弧 / 紫余弧，呼应精简视图 conic 环形）+ 中央 token 火花 + 底部三色用量柱
// 配色对齐 renderer/css/theme.css：--accent #2dd4bf / --violet #8b5cf6 / --amber #fb923c
// 渲染采用 4x4 超采样抗锯齿，边缘平滑。

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

// ---------- 绘制 ----------
const C = {
  teal: [0x2d, 0xd4, 0xbf],   // --accent
  tealEnd: [0x34, 0xd3, 0x99],
  cyan: [0x22, 0xd3, 0xee],
  violet: [0x8b, 0x5c, 0xf6], // --violet
  amber: [0xfb, 0x92, 0x3c],  // --amber
  bgTop: [0x0e, 0x11, 0x20],
  bgBot: [0x1a, 0x20, 0x36],
  white: [0xf4, 0xf6, 0xfb],
};
const lerp = (a, b, t) => a + (b - a) * t;
const lerp3 = (a, b, t) => [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)];

// 点是否在多边形内（射线法）
function inPoly(px, py, pts) {
  let inside = false;
  for (let i = 0, j = pts.length - 1; i < pts.length; j = i++) {
    const xi = pts[i][0], yi = pts[i][1], xj = pts[j][0], yj = pts[j][1];
    if (((yi > py) !== (yj > py)) && (px < ((xj - xi) * (py - yi)) / (yj - yi) + xi)) inside = !inside;
  }
  return inside;
}

function inRoundRect(x, y, x0, y0, w, h, rad) {
  const cx = Math.min(Math.max(x, x0 + rad), x0 + w - rad);
  const cy = Math.min(Math.max(y, y0 + rad), y0 + h - rad);
  const dx = x - cx, dy = y - cy;
  return dx * dx + dy * dy <= rad * rad;
}

function drawIcon(size) {
  const SS = 4;                 // 超采样倍数
  const S = size * SS;
  const buf = Buffer.alloc(S * S * 4);

  const s = size;
  const tileR = s * 0.2355;     // 圆角半径
  const ringCx = s * 0.5;
  const ringCy = s * 0.49;
  const ringR = s * 0.29;       // 环半径（中心线）
  const ringHW = s * 0.043;     // 环半带宽
  const glowW = s * 0.058;      // 环外光晕宽度
  const arcMain = 0.72;         // 主弧占比（青→绿，代表剩余额度）
  const borderHW = Math.max(1, s * 0.003);

  // 底部三根用量柱
  const bars = [
    { h: 0.62, c: C.teal },
    { h: 0.42, c: C.violet },
    { h: 0.26, c: C.amber },
  ];
  const barW = s * 0.086;
  const barGap = s * 0.055;
  const barR = Math.max(1, s * 0.012);
  const barBaseY = s * 0.88;
  const barMaxH = s * 0.10;
  const barX0 = (s - (barW * 3 + barGap * 2)) / 2;

  // 中央 token 火花（闪电多边形，坐标以 0.5,0.5 归一化再缩放）
  const boltH = s * 0.225;
  const boltCx = s * 0.5;
  const boltCy = s * 0.465;
  const bolt = [
    [0.56, 0.0], [0.20, 0.52], [0.44, 0.52], [0.32, 1.0], [0.82, 0.40], [0.56, 0.40],
  ].map(([px, py]) => [boltCx + (px - 0.5) * boltH, boltCy + (py - 0.5) * boltH]);
  const boltTop = Math.min(...bolt.map((p) => p[1]));
  const boltBot = Math.max(...bolt.map((p) => p[1]));

  const sampleColor = (x, y) => {
    // 圆角块 SDF（负值在块内）
    const rx = Math.min(Math.max(x, tileR), s - tileR);
    const ry = Math.min(Math.max(y, tileR), s - tileR);
    const dTile = Math.hypot(x - rx, y - ry) - tileR;
    if (dTile > 0) return null;

    let c = lerp3(C.bgTop, C.bgBot, y / s);
    const dc = Math.hypot(x - ringCx, y - ringCy);
    // 中心柔和辉光
    c = lerp3(c, C.teal, Math.max(0, 1 - dc / (ringR * 1.25)) * 0.10);
    // 环形光晕（半透明青色，随距离衰减）
    const dr = Math.abs(dc - ringR) - ringHW;
    c = lerp3(c, C.teal, Math.max(0, 1 - Math.max(dr, 0) / glowW) * 0.28);
    // 环形本体：主弧青→绿（自顶部顺时针），余弧紫（代表已用）
    if (dr <= 0) {
      let a = Math.atan2(y - ringCy, x - ringCx) + Math.PI / 2;
      if (a < 0) a += Math.PI * 2;
      const t = a / (Math.PI * 2);
      c = t < arcMain
        ? lerp3(C.cyan, C.tealEnd, t / arcMain)
        : lerp3(c, C.violet, 0.85);
    }
    // 中央火花
    if (dc < ringR - ringHW && inPoly(x, y, bolt)) {
      const t = Math.min(1, Math.max(0, (y - boltTop) / (boltBot - boltTop)));
      c = lerp3(C.white, [0x5e, 0xea, 0xd4], t);
    }
    // 底部用量柱
    for (let i = 0; i < bars.length; i++) {
      const b = bars[i];
      const bh = barMaxH * b.h;
      const top = barBaseY - bh;
      if (x >= barX0 + i * (barW + barGap) - barR && x <= barX0 + i * (barW + barGap) + barW + barR
        && y >= top && y <= barBaseY && inRoundRect(x, y, barX0 + i * (barW + barGap), top, barW, bh, barR)) {
        c = lerp3(b.c, C.white, ((y - top) / bh) * 0.35);
      }
    }
    // 玻璃高亮描边
    if (Math.abs(dTile) < borderHW) c = lerp3(c, C.white, 0.15);
    return c;
  };

  // 逐采样点绘制
  for (let sy = 0; sy < S; sy++) {
    const y = (sy + 0.5) / SS;
    for (let sx = 0; sx < S; sx++) {
      const x = (sx + 0.5) / SS;
      const col = sampleColor(x, y);
      const i = (sy * S + sx) * 4;
      if (col) {
        buf[i] = col[0]; buf[i + 1] = col[1]; buf[i + 2] = col[2]; buf[i + 3] = 255;
      } else {
        buf[i + 3] = 0;
      }
    }
  }

  // 4x4 块平均 → 目标尺寸（抗锯齿）
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let dy = 0; dy < SS; dy++) {
        for (let dx = 0; dx < SS; dx++) {
          const i = ((y * SS + dy) * S + x * SS + dx) * 4;
          r += buf[i]; g += buf[i + 1]; b += buf[i + 2]; a += buf[i + 3];
        }
      }
      const n = SS * SS;
      const i = (y * size + x) * 4;
      out[i] = Math.round(r / n);
      out[i + 1] = Math.round(g / n);
      out[i + 2] = Math.round(b / n);
      out[i + 3] = Math.round(a / n);
    }
  }
  return encodePng(size, size, out);
}

// ---------- 输出 ----------
const outDir = path.join(__dirname, '..', 'resources');
fs.mkdirSync(outDir, { recursive: true });
const writes = [
  ['icon-1024.png', drawIcon(1024)],
  ['icon.png', drawIcon(32)],
  ['tray.png', drawIcon(16)],
  ['logo-preview.png', drawIcon(512)],
];
for (const [name, png] of writes) {
  const p = path.join(outDir, name);
  fs.writeFileSync(p, png);
  console.log('已生成:', p, `(${png.length} bytes)`);
}
