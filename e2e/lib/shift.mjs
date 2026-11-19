// 两帧之间的竖直位移估计：用行 hash 在中间区域找使“相同行数”最多的 dy（正 = 内容向上移了 dy 像素）。
// 只看左侧 colsMax 列（避开右缘滚动条），纯色行（信息量低）不计票——对应 doc/40 里“纯色行会产生伪匹配”。
export function rowHashes(img, colsMax = 1000) {
  const { w, h, data } = img, n = Math.min(w, colsMax), hs = new Array(h), info = new Uint8Array(h)
  for (let y = 0; y < h; y++) {
    let hh = 2166136261, first = -1, distinct = 0
    for (let x = 0; x < n; x++) {
      const o = (y * w + x) * 4, v = data[o] | (data[o + 1] << 8) | (data[o + 2] << 16)
      if (first < 0) first = v; else if (v !== first) distinct = 1
      hh = Math.imul(hh ^ v, 16777619)
    }
    hs[y] = hh; info[y] = distinct
  }
  return { hs, info }
}

export function estimateShift(a, b, { maxShift = 1200, lo = 0.1, hi = 0.9 } = {}) {
  const A = rowHashes(a), B = rowHashes(b), h = a.h
  let best = { dy: 0, votes: -1 }
  for (let dy = -maxShift; dy <= maxShift; dy++) {
    let v = 0
    for (let y = Math.floor(h * lo); y < Math.floor(h * hi); y++) {
      const y2 = y - dy   // a 的第 y 行 出现在 b 的第 y-dy 行（内容上移 dy）
      if (y2 < 0 || y2 >= h || !A.info[y]) continue
      if (A.hs[y] === B.hs[y2]) v++
    }
    if (v > best.votes) best = { dy, votes: v }
  }
  return best
}
