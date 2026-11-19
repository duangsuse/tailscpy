// 活镜 A 端无损帧解码器（与 relay/bench/codec.py 的 Decoder 逐操作对应）。经典脚本，<script src> 即可。
// 帧缓冲是 RGBA 字节数组（alpha 恒 255），所有操作都是字节数组运算，只有 WebP 字面量借用浏览器解码：
//   createImageBitmap(..., {colorSpaceConversion:'none', premultiplyAlpha:'none'}) → 软件 canvas → getImageData，保证不做色彩转换。
// 操作码：1 FRAME_REF  2 COPY  3 SPRITE_REF  4 LIT(webp)  5 REG  6 FILL  7 GLYPH_ATLAS(webp)  8 GLYPH_RUN  9 GREG(从帧缓冲切字形登记)  10 DIM(整块压暗/泛白，16 位权重)
class HjDecoder {
  constructor(W, H, histN = 16) {
    this.W = W; this.H = H; this.histN = histN;
    this.fb = new Uint8ClampedArray(W * H * 4); for (let i = 3; i < this.fb.length; i += 4) this.fb[i] = 255;
    this.hist = new Map(); this.nextSlot = 0; this.pool = []; this.glyphs = []; this.gseen = new Set();
    this.scratch = null; this.dirty = null;   // dirty = [x0,y0,x1,y1]：本帧触及的外接矩形，供显示层只刷这块
  }
  reset() { this.fb.fill(0); for (let i = 3; i < this.fb.length; i += 4) this.fb[i] = 255; this.hist.clear(); this.pool = []; this.glyphs = []; this.gseen.clear(); }
  _touch(x, y, w, h) {
    const d = this.dirty; if (!d) this.dirty = [x, y, x + w, y + h];
    else { d[0] = Math.min(d[0], x); d[1] = Math.min(d[1], y); d[2] = Math.max(d[2], x + w); d[3] = Math.max(d[3], y + h); }
  }
  _remember() {
    this.hist.set(this.nextSlot++, this.fb.slice());
    for (const k of this.hist.keys()) if (k < this.nextSlot - this.histN) this.hist.delete(k);
  }
  _blit(src, sw, sx, sy, dx, dy, w, h, dst = this.fb, dw = this.W) {
    for (let r = 0; r < h; r++) {
      const s = ((sy + r) * sw + sx) * 4, d = ((dy + r) * dw + dx) * 4;
      dst.set(src.subarray(s, s + w * 4), d);
    }
  }
  async _webp(bytes, w, h) {
    const bmp = await createImageBitmap(new Blob([bytes], {type: 'image/webp'}), {colorSpaceConversion: 'none', premultiplyAlpha: 'none'});
    if (!this.scratch) this.scratch = document.createElement('canvas').getContext('2d', {willReadFrequently: true, colorSpace: 'srgb'});
    const c = this.scratch.canvas; w = c.width = bmp.width; h = c.height = bmp.height;   // 尺寸取自位图本身（图集事先不知道高）
    this.scratch.clearRect(0, 0, w, h); this.scratch.drawImage(bmp, 0, 0); bmp.close();
    return this.scratch.getImageData(0, 0, w, h, {colorSpace: 'srgb'}).data;
  }
  // 字形去重键：像素字节(RGB)+高宽低 8 位，latin1 解码是单射，Map/Set 做精确比较
  _gkey(rgba, w, h) {
    const rgb = new Uint8Array(w * h * 3 + 2);
    for (let i = 0, j = 0; i < w * h * 4; i += 4) { rgb[j++] = rgba[i]; rgb[j++] = rgba[i + 1]; rgb[j++] = rgba[i + 2]; }
    rgb[w * h * 3] = h & 255; rgb[w * h * 3 + 1] = w & 255;
    return new TextDecoder('latin1').decode(rgb);
  }
  _addGlyph(rgba, w, h) { const k = this._gkey(rgba, w, h); if (this.gseen.has(k)) return; this.gseen.add(k); this.glyphs.push({w, h, data: rgba}); }
  // 与 codec.py 的 rgb_segments 一致：按「非底色像素」的空列切字，上下裁紧
  _segments(x, y, w, h, bg) {
    const W = this.W, non = new Uint8Array(w * h), colAny = new Uint8Array(w);
    for (let j = 0; j < h; j++) for (let i = 0; i < w; i++) {
      const o = ((y + j) * W + x + i) * 4;
      if (this.fb[o] !== bg[0] || this.fb[o + 1] !== bg[1] || this.fb[o + 2] !== bg[2]) { non[j * w + i] = 1; colAny[i] = 1; }
    }
    const out = []; let i = 0;
    while (i < w) {
      if (!colAny[i]) { i++; continue; }
      let i2 = i; while (i2 < w && colAny[i2]) i2++;      // 连续的非空列 = 一段（空列≥1 即切开）
      let y0 = h, y1 = -1;
      for (let j = 0; j < h; j++) for (let c = i; c < i2; c++) if (non[j * w + c]) { if (j < y0) y0 = j; if (j > y1) y1 = j; }
      const gw = i2 - i, gh = y1 - y0 + 1, data = new Uint8ClampedArray(gw * gh * 4);
      for (let j = 0; j < gh; j++) for (let c = 0; c < gw; c++) { const o = ((y + y0 + j) * W + x + i + c) * 4, d = (j * gw + c) * 4; data[d] = this.fb[o]; data[d + 1] = this.fb[o + 1]; data[d + 2] = this.fb[o + 2]; data[d + 3] = 255; }
      out.push({w: gw, h: gh, data}); i = i2;
    }
    return out;
  }
  // 返回 true 表示画面有变化（需要刷新）
  async apply(buf) {
    if (!buf.length) return false;
    const rd = () => { let n = 0, m = 1, b; do { b = buf[p++]; n += (b & 127) * m; m *= 128; } while (b & 128); return n; };
    const unzz = n => (n % 2 ? -(n + 1) / 2 : n / 2);
    let p = 0; this.dirty = null; const W = this.W, H = this.H;
    if (buf[0] === 1) { p = 1; this.fb.set(this.hist.get(rd())); this._touch(0, 0, W, H); this._remember(); return true; }
    const snap = this.fb.slice(), reg = [], greg = [];
    while (p < buf.length) {
      const tag = buf[p++];
      if (tag === 2) {
        const sx = rd(), sy = rd(), dx = rd(), dy = rd(), w = rd(), h = rd();
        this._blit(snap, W, sx, sy, dx, dy, w, h); this._touch(dx, dy, w, h);
      } else if (tag === 3) {
        const id = rd(), x = rd(), y = rd(), s = this.pool[id];
        this._blit(s.data, s.w, 0, 0, x, y, s.w, s.h); this._touch(x, y, s.w, s.h);
      } else if (tag === 4) {
        const x = rd(), y = rd(), w = rd(), h = rd(), len = rd();
        if (buf[p] !== 0) throw new Error('JS 解码器只支持 WebP 字面量(codec id 0)'); const px = await this._webp(buf.subarray(p + 1, p + len), w, h); p += len;
        this._blit(px, w, 0, 0, x, y, w, h); this._touch(x, y, w, h);
      } else if (tag === 5) reg.push([rd(), rd(), rd(), rd()]);
      else if (tag === 6) {
        const x = rd(), y = rd(), w = rd(), h = rd(), r = buf[p], g = buf[p + 1], b = buf[p + 2]; p += 3;
        for (let j = 0; j < h; j++) for (let i = 0; i < w; i++) { const o = ((y + j) * W + x + i) * 4; this.fb[o] = r; this.fb[o + 1] = g; this.fb[o + 2] = b; }
        this._touch(x, y, w, h);
      } else if (tag === 7) {
        const n = rd(), sizes = []; for (let k = 0; k < n; k++) { const w = rd(), h = rd(); sizes.push([h, w]); }
        const len = rd(); if (buf[p] !== 0) throw new Error('JS 解码器只支持 WebP 图集(codec id 0)'); const atlas = await this._webp(buf.subarray(p + 1, p + len), 1024, 0); p += len;   // 高由图片本身决定
        const aw = this.scratch.canvas.width; let x = 0, y = 0, rowh = 0;
        for (const [h, w] of sizes) {                       // 与 codec.py pack_atlas 相同的搁板装箱
          if (x + w > 1024) { x = 0; y += rowh; rowh = 0; }
          const data = new Uint8ClampedArray(w * h * 4);
          for (let j = 0; j < h; j++) data.set(atlas.subarray(((y + j) * aw + x) * 4, ((y + j) * aw + x + w) * 4), j * w * 4);
          this._addGlyph(data, w, h); x += w; rowh = Math.max(rowh, h);
        }
      } else if (tag === 8) {
        const n = rd(); let px = 0, py = 0;
        for (let k = 0; k < n; k++) {
          const gid = rd(); px += unzz(rd()); py += unzz(rd()); const g = this.glyphs[gid];
          this._blit(g.data, g.w, 0, 0, px, py, g.w, g.h); this._touch(px, py, g.w, g.h);
        }
      } else if (tag === 10) {
        const x = rd(), y = rd(), w = rd(), h = rd(), F = rd(), c = [buf[p], buf[p + 1], buf[p + 2]]; p += 3;   // out = (b·F + c·(65536−F) + 32768) >> 16
        for (let j = 0; j < h; j++) for (let i = 0; i < w; i++) {
          const o = ((y + j) * W + x + i) * 4;
          for (let k = 0; k < 3; k++) this.fb[o + k] = Math.floor((this.fb[o + k] * F + c[k] * (65536 - F) + 32768) / 65536);
        }
        this._touch(x, y, w, h);
      } else if (tag === 9) {
        const x = rd(), y = rd(), w = rd(), h = rd(); greg.push([x, y, w, h, [buf[p], buf[p + 1], buf[p + 2]]]); p += 3;
      } else throw new Error('bad tag ' + tag);
    }
    for (const [x, y, w, h, bg] of greg) for (const g of this._segments(x, y, w, h, bg)) this._addGlyph(g.data, g.w, g.h);
    for (const [x, y, w, h] of reg) {
      const data = new Uint8ClampedArray(w * h * 4); this._blit(this.fb, W, x, y, 0, 0, w, h, data, w); this.pool.push({w, h, data});
    }
    this._remember(); return true;
  }
}
