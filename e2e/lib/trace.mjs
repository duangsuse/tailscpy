// 语料录制（host 的 Recorder.kt 写出，80 §3）的读取：manifest、events、按帧号还原整帧。
import fs from 'node:fs'
import path from 'node:path'
import zlib from 'node:zlib'

export function openTrace(dir) {
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'))
  const events = fs.readFileSync(path.join(dir, 'events.jsonl'), 'utf8').split('\n').filter(Boolean).map(l => JSON.parse(l))
  const idxB = fs.readFileSync(path.join(dir, 'frames.idx')), bin = fs.readFileSync(path.join(dir, 'frames.bin'))
  const idx = []   // {seq, t, off}
  for (let i = 0; i + 16 <= idxB.length; i += 16) idx.push({ seq: idxB.readUInt32LE(i), t: idxB.readUInt32LE(i + 4), off: Number(idxB.readBigUInt64LE(i + 8)) })
  const { w, h } = manifest, rb = w * 4

  const rec = off => {   // frames.bin 记录：seq u32, kind u8, rawLen u32, zLen u32, z[zLen]
    const kind = bin[off + 4], rawLen = bin.readUInt32LE(off + 5), zLen = bin.readUInt32LE(off + 9)
    const raw = zlib.inflateSync(bin.subarray(off + 13, off + 13 + zLen))
    if (raw.length !== rawLen) throw new Error('帧记录长度不符')
    return { kind, raw }
  }
  /** 还原 seq ≤ 目标的最近一帧（从它之前的最近关键帧起逐条应用增量）。返回 {seq, t, w, h, data}。 */
  function frame(target) {
    let last = -1; for (let i = 0; i < idx.length; i++) if (idx[i].seq <= target) last = i
    if (last < 0) throw new Error(`没有 seq ≤ ${target} 的帧`)
    let k = last; while (k > 0 && rec(idx[k].off).kind !== 0) k--
    const data = Buffer.alloc(w * h * 4)
    for (let i = k; i <= last; i++) {
      const { kind, raw } = rec(idx[i].off)
      if (kind === 0) raw.copy(data)
      else for (let p = 0; p < raw.length;) { const y0 = raw.readUInt16BE(p), n = raw.readUInt16BE(p + 2); raw.copy(data, y0 * rb, p + 4, p + 4 + n * rb); p += 4 + n * rb }
    }
    return { seq: idx[last].seq, t: idx[last].t, w, h, data }
  }
  return { manifest, events, idx, frame, trees: dir }
}
