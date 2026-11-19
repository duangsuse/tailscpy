// 最小 PNG 解码（8 位 RGB / RGBA，非隔行；host 的 Bitmap.compress 产出的是这种）。返回 {w,h,data:RGBA}。
import zlib from 'node:zlib'

export function decodePng(buf) {
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error('不是 PNG')
  let p = 8, w = 0, h = 0, ct = 0; const idat = []
  while (p < buf.length) {
    const len = buf.readUInt32BE(p), type = buf.toString('latin1', p + 4, p + 8), d = buf.subarray(p + 8, p + 8 + len)
    if (type === 'IHDR') { w = d.readUInt32BE(0); h = d.readUInt32BE(4); if (d[8] !== 8 || d[12] !== 0) throw new Error('只支持 8 位非隔行'); ct = d[9] }
    else if (type === 'IDAT') idat.push(d)
    else if (type === 'IEND') break
    p += 12 + len
  }
  const bpp = ct === 6 ? 4 : ct === 2 ? 3 : 0
  if (!bpp) throw new Error('只支持 RGB / RGBA，colorType=' + ct)
  const raw = zlib.inflateSync(Buffer.concat(idat)), stride = w * bpp
  const out = Buffer.alloc(w * h * 4), cur = Buffer.alloc(stride); let prev = Buffer.alloc(stride)
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)], src = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1))
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? cur[i - bpp] : 0, b = prev[i], c = i >= bpp ? prev[i - bpp] : 0
      let v = src[i]
      if (f === 1) v += a; else if (f === 2) v += b; else if (f === 3) v += (a + b) >> 1
      else if (f === 4) { const pa = Math.abs(b - c), pb = Math.abs(a - c), pc = Math.abs(a + b - 2 * c); v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c }
      cur[i] = v & 255
    }
    for (let x = 0; x < w; x++) { const o = (y * w + x) * 4; out[o] = cur[x * bpp]; out[o + 1] = cur[x * bpp + 1]; out[o + 2] = cur[x * bpp + 2]; out[o + 3] = bpp === 4 ? cur[x * bpp + 3] : 255 }
    prev = Buffer.from(cur)
  }
  return { w, h, data: out }
}

/** 两张图的差异：不同像素数、不同行数、不同像素的外包矩形。尺寸不同则 sizeMismatch。 */
export function diffPng(a, b, ignore = []) {
  if (a.w !== b.w || a.h !== b.h) return { sizeMismatch: true, pixels: Infinity, rows: Infinity, ratio: 1 }
  let px = 0, rows = 0, x0 = a.w, y0 = a.h, x1 = -1, y1 = -1
  const skip = new Uint8Array(a.h); for (const [y0, y1] of ignore) for (let y = Math.max(0, y0); y < Math.min(a.h, y1); y++) skip[y] = 1
  for (let y = 0; y < a.h; y++) {
    if (skip[y]) continue
    let any = false
    for (let x = 0; x < a.w; x++) {
      const o = (y * a.w + x) * 4
      if (a.data[o] !== b.data[o] || a.data[o + 1] !== b.data[o + 1] || a.data[o + 2] !== b.data[o + 2]) { px++; any = true; if (x < x0) x0 = x; if (x > x1) x1 = x; if (y < y0) y0 = y; if (y > y1) y1 = y }
    }
    if (any) rows++
  }
  return { pixels: px, rows, ratio: px / (a.w * a.h), bbox: px ? [x0, y0, x1 + 1, y1 + 1] : null }
}

const crcT = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0 } return t })()
const crc = b => { let c = 0xffffffff; for (const x of b) c = crcT[(c ^ x) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0 }

/** RGBA → PNG（8 位 RGBA，filter 0）。 */
export function encodePng({ w, h, data }) {
  const raw = Buffer.alloc((w * 4 + 1) * h)
  for (let y = 0; y < h; y++) data.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4)
  const chunk = (type, body) => { const b = Buffer.alloc(12 + body.length); b.writeUInt32BE(body.length, 0); b.write(type, 4, 'latin1'); body.copy(b, 8); b.writeUInt32BE(crc(b.subarray(4, 8 + body.length)), 8 + body.length); return b }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 6
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw, { level: 3 })), chunk('IEND', Buffer.alloc(0))])
}
