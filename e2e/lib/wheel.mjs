// 滚轮原语：找主滚动容器、标定有效因子 F_eff（像素/格）、按精确像素公式滚 N 像素（P1 探针的结论）。
import { sleep } from './host.mjs'
import { decodePng } from './png.mjs'
import { estimateShift } from './shift.mjs'

export const shotOf = async h => decodePng(Buffer.from((await h.ok({ cmd: 'frame', fmt: 'png' })).data, 'base64'))

/** 虚拟屏上最大的可滚动节点；x 取容器中心偏左 40px（避开右缘滚动条）。 */
export async function mainScrollable(h) {
  const t = (await h.ok({ cmd: 'tree', full: true })).tree
  let best = null
  const walk = n => {
    if (n.scroll && !n.hidden && !/ViewPager|Horizontal|Pager/i.test((n.cls || "") + (n.id || ""))) { const [l, tp, r, b] = n.b, a = (r - l) * (b - tp); if (!best || a > best.a) best = { n, a } }
    for (const k of n.kids || []) walk(k)
  }
  for (const w of t.windows) if (w.root) walk(w.root)
  if (!best) return null
  const [l, tp, r, b] = best.n.b
  return { id: best.n.id, b: best.n.b, x: Math.round((l + r) / 2) - 40, y: Math.round((tp + b) / 2), rows: best.n.coll?.rows }
}

/** v=-1 一格，量实际位移 → F_eff。每个容器的实际因子可能和 ViewConfiguration 报的不同（设置首页实测 ≈2.12 倍）。 */
export async function calibrate(h, c, settle) {
  const a = await shotOf(h)
  await h.ok({ cmd: 'wheel', x: c.x, y: c.y, v: -1, settle })
  await sleep(200)
  const dy = estimateShift(a, await shotOf(h)).dy
  return Math.abs(dy) >= 2 ? Math.abs(dy) : null   // null = 该容器不响应滚轮
}

/** 往下滚 px 像素（负数 = 往回滚），按 ≤ chunk 一步，每步等静止。 */
export async function scrollBy(h, c, F, px, settle, chunk = 300) {
  let left = Math.abs(px); const sgn = px >= 0 ? -1 : 1   // 内容上移（往下看）= v 为负
  while (left > 0) {
    const n = Math.min(chunk, left); left -= n
    await h.ok({ cmd: 'wheel', x: c.x, y: c.y, v: sgn * (n + 0.5) / F, settle })
  }
}
