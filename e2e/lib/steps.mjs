// 录制器和回放器共用：手势分类与注入。录制 = 回放走同一份代码，所以同一手势的结果可复现。
export const SLOP = 48      // 设备像素（≈ 屏幕上 18px）：按下后位移不超过它 = 点按 / 长按；一旦超过，就是从按下到松手的整段拖动
export const LONG_MS = 500  // 不动且按住超过它就算长按
export const STEP_MS = 30   // 滑动轨迹重采样间隔
export const MAX_STEP_PX = 60
export const HOLD_MS = 60   // 滑动抬手前在终点停多久：Android 的 VelocityTracker 把 >40ms 前的样本当作“已停住”，速度为 0 → 没有惯性

/** 浏览器给的指针轨迹 [[dt, x, y], ...]（dt 相对按下，设备像素）→ 步骤（不含选择器）。 */
export function classify(pts) {
  const [, x0, y0] = pts[0], [dt, x1, y1] = pts.at(-1)
  const moved = pts.some(([, x, y]) => Math.hypot(x - x0, y - y0) > SLOP)
  if (!moved) return dt < LONG_MS ? { ev: 'tap', x: x0, y: y0 } : { ev: 'long', x: x0, y: y0, ms: dt }
  // 重采样成固定 STEP_MS 间隔（线性插值，保留总时长）：事件间隔短于一帧时，系统的批处理会让同一轨迹的结果每次略有不同
  // （实测 14ms 间隔两次回放差 7%，30ms 间隔 4 次逐像素一致）。录制与回放都用这条规整后的轨迹。
  // 同时限速：每个采样步位移不超过 MAX_STEP_PX，超了就把整个手势拉长（实验里每步 50px 稳定，107px 不稳定）
  let plen = 0; for (let i = 1; i < pts.length; i++) plen += Math.hypot(pts[i][1] - pts[i - 1][1], pts[i][2] - pts[i - 1][2])
  const T = Math.max(dt, Math.ceil(plen / MAX_STEP_PX) * STEP_MS), sc = dt / T, at = tt => {
    const t = tt * sc
    let i = 1; while (i < pts.length - 1 && pts[i][0] < t) i++
    const [ta, xa, ya] = pts[i - 1], [tb, xb, yb] = pts[i], f = tb > ta ? Math.min(1, Math.max(0, (t - ta) / (tb - ta))) : 1
    return [Math.round(xa + (xb - xa) * f), Math.round(ya + (yb - ya) * f)]
  }
  const out = []
  for (let t = 0; t < T; t += STEP_MS) out.push([t, ...at(t)])
  out.push([T, Math.round(x1), Math.round(y1)])
  out.push([T + HOLD_MS, Math.round(x1), Math.round(y1)])
  return { ev: 'swipe', x1: x0, y1: y0, x2: x1, y2: y1, ms: T + HOLD_MS, pts: out }
}

/** 按步骤注入。extra = {settle, stack, shot}；tap/long 可传坐标覆盖（回放里按选择器重新定位）。 */
export function inject(h, s, extra, at = [s.x, s.y]) {
  switch (s.ev) {
    case 'tap': return h.call({ cmd: 'tap', x: at[0], y: at[1], ...extra })
    case 'long': return h.call({ cmd: 'long', x: at[0], y: at[1], ms: s.ms, ...extra })
    case 'swipe': return h.call({ cmd: 'path', pts: s.pts, ...extra })
    case 'back': return h.call({ cmd: 'back', ...extra })
    default: throw new Error('未知事件 ' + s.ev)
  }
}

// ── jsonl 的行格式（磁盘上）↔ 内存里的步骤 ──
// 第 1 行是 meta；之后第 i 行就是第 i 步（行号推定，不写 type / i / shot；截图是 <用例名>/NNNN.png）。
//   {"Open":"pkg"}  {"Tap":"@x,y"}  {"Long":"@x,y","ms":700}  {"Swipe":["@x1,y1","@x2,y2"],"pts":[[dt,x,y],…]}  {"Back":true}
//   附加键：sel（点按命中的节点）、res{tChange,tSettle,rows,volatile}（0 = 没变化 / 没静止）、stack[深度,栈顶]、wait（只写和 meta.wait 不同的键）
const at = (x, y) => `@${x},${y}`
const pt = p => p.slice(1).split(',').map(Number)

/** 内存步骤（ev,x,y,…，res 为 host 的 settle 结果）→ 一行。metaWait 用来省掉和 meta 相同的 wait 键。 */
export function encodeStep(s, metaWait = {}) {
  const o = {}
  if (s.ev === 'open') o.Open = s.pkg
  else if (s.ev === 'tap') o.Tap = at(s.x, s.y)
  else if (s.ev === 'long') { o.Long = at(s.x, s.y); o.ms = s.ms }
  else if (s.ev === 'swipe') { o.Swipe = [at(s.x1, s.y1), at(s.x2, s.y2)]; o.pts = s.pts }
  else if (s.ev === 'back') o.Back = true
  else throw new Error('未知事件 ' + s.ev)
  if (s.sel) o.sel = s.sel
  if (s.res) { const r = s.res; o.res = { tChange: r.changed ? Math.max(1, r.tChange) : 0, tSettle: r.settled ? Math.max(1, r.tSettle) : 0, rows: r.rows, ...(r.volatile && { volatile: r.volatile }) } }
  if (s.stack) o.stack = [s.stack.depth, s.stack.top]
  const w = {}; for (const [k, v] of Object.entries(s.wait || {})) if (JSON.stringify(v) !== JSON.stringify(metaWait[k])) w[k] = v
  if (Object.keys(w).length) o.wait = w
  return o
}

/** 一行 → 内存步骤；i = 行号推定的步号。changed / settled 由 tChange / tSettle == 0 推出。 */
export function decodeStep(l, i, metaWait = {}) {
  const s = { i }
  if ('Open' in l) { s.ev = 'open'; s.pkg = l.Open }
  else if ('Tap' in l) { s.ev = 'tap'; [s.x, s.y] = pt(l.Tap) }
  else if ('Long' in l) { s.ev = 'long'; [s.x, s.y] = pt(l.Long); s.ms = l.ms }
  else if ('Swipe' in l) { s.ev = 'swipe'; [s.x1, s.y1] = pt(l.Swipe[0]); [s.x2, s.y2] = pt(l.Swipe[1]); s.pts = l.pts; s.ms = l.pts.at(-1)[0] }
  else if ('Back' in l) s.ev = 'back'
  else throw new Error(`第 ${i} 步看不懂：${JSON.stringify(l).slice(0, 80)}`)
  if (l.sel) s.sel = l.sel
  if (l.res) s.res = { ...l.res, changed: l.res.tChange !== 0, settled: l.res.tSettle !== 0 }
  if (l.stack) s.stack = { depth: l.stack[0], top: l.stack[1] }
  s.wait = { ...metaWait, ...l.wait }
  return s
}

/** 步号 → 截图文件名（相对用例目录的上一级，即 <用例名>/NNNN.png）。 */
export const shotName = (name, i) => `${name}/${String(i).padStart(4, '0')}.png`
