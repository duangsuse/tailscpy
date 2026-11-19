// 回放一个人工录制的 case（录制器网页与 e2e/run.mjs 共用）。
import fs from 'node:fs'
import path from 'node:path'
import { decodePng, diffPng } from './png.mjs'
import { inject } from './steps.mjs'

export function loadCase(file) {
  const ls = fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map(l => JSON.parse(l))
  const meta = ls.find(l => l.type === 'meta'); if (!meta) throw new Error(file + ' 没有 meta 行')
  return { file, dir: path.dirname(file), meta, steps: ls.filter(l => l.type === 'step') }
}

function findNode(tree, sel, near) {
  const cands = []
  const walk = n => {
    if (!n.hidden && (!sel.id || n.id === sel.id) && (!sel.text || n.text === sel.text) && (!sel.desc || n.desc === sel.desc) && (!sel.cls || n.cls === sel.cls)
      && (sel.row === undefined || n.item?.row === sel.row)) cands.push(n)
    for (const k of n.kids || []) walk(k)
  }
  for (const w of tree.windows) if (w.root) walk(w.root)
  const c = n => { const [l, t, r, b] = n.b; return Math.hypot((l + r) / 2 - near[0], (t + b) / 2 - near[1]) }
  return cands.sort((a, b) => c(a) - c(b))[0]
}

async function replayStep(h, c, s, notes) {
  const wait = s.wait || c.meta.wait
  const settle = { change: wait.change, settle_ms: wait.settle_ms, timeout_ms: wait.timeout_ms, ignore: wait.ignore || [] }
  const base = { settle, stack: true, shot: true }
  if (s.ev === 'tap' || s.ev === 'long') {
    let [x, y] = [s.x, s.y]
    if (s.sel && (s.sel.id || s.sel.text || s.sel.desc)) {
      const t = await h.ok({ cmd: 'tree', full: false })
      const n = findNode(t.tree, s.sel, [x, y])
      if (!n) notes.push('选择器未命中，用录制坐标')
      else {
        const [l, tp, r, b] = n.b, [ol, ot, or, ob] = s.sel.b
        if (l !== ol || tp !== ot || r !== or || b !== ob) { x = Math.round(l + (x - ol) / (or - ol) * (r - l)); y = Math.round(tp + (y - ot) / (ob - ot) * (b - tp)); notes.push(`节点位置变化，点 (${x},${y})`) }
      }
    }
    return inject(h, s, base, [x, y])
  }
  return inject(h, s, base)
}

/** 回放一个 case。opts: {tol, outDir（逐步截图写到 outDir/<包名>/）, onStep(结果)}；每步结果带 {i, ev, ok, why, notes, ms, diff, got（回放截图相对路径）}。 */
export async function runCase(h, c, { tol = 0.001, outDir, onStep = () => {} } = {}) {
  const name = path.basename(c.file, '.jsonl'), out = path.join(outDir, name); fs.mkdirSync(out, { recursive: true })
  const push = (arr, x) => { arr.push(x); onStep(x) }
  const results = [], shots = []
  const o = await h.call({ cmd: 'open', pkg: c.meta.pkg, w: c.meta.w, h: c.meta.h, dpi: c.meta.dpi })
  if (!o.ok) { const x = { i: 0, ev: 'open', ok: false, why: 'open 失败: ' + o.err }; push(results, x); return { name, results, shots } }
  for (const s of c.steps) {
    const notes = [], t0 = Date.now()
    let r
    if (s.ev === 'open') r = await h.call({ cmd: 'settle', settle: { change: s.wait?.change ?? 0.005, settle_ms: s.wait?.settle_ms ?? 700, timeout_ms: s.wait?.timeout_ms ?? 700, ignore: s.wait?.ignore || [] }, stack: true, shot: true })
    else r = await replayStep(h, c, s, notes)
    if (!r.ok) { push(results, { i: s.i, ev: s.ev, ok: false, why: r.err }); break }
    const got = Buffer.from(r.shot, 'base64'); const gotName = String(s.i).padStart(4, '0') + '.png'; fs.writeFileSync(path.join(out, gotName), got)
    const gotImg = decodePng(got); shots.push(gotImg)
    const want = decodePng(fs.readFileSync(path.join(c.dir, s.shot)))
    const d = diffPng(want, gotImg, s.wait?.ignore || []), why = []
    // open 步骤只等固定时间、不等静止（首屏常在加载 / 走马灯），所以它的画面差异只作说明，不判失败
    if (d.ratio > tol && s.ev === 'open') notes.push(`open 画面差异 ${(d.ratio * 100).toFixed(3)}%（不判失败）`)
    else if (d.ratio > tol) why.push(`画面不同 ${d.pixels}px (${(d.ratio * 100).toFixed(3)}%) 行 ${d.rows} bbox ${JSON.stringify(d.bbox)}`)
    if (s.stack && r.stack && s.stack.depth !== r.stack.depth) why.push(`栈深 ${r.stack.depth}≠录制 ${s.stack.depth}`)
    if (s.res && r.settle && s.res.changed !== r.settle.changed) why.push(`变化 ${r.settle.changed}≠录制 ${s.res.changed}`)
    if (r.settle && !r.settle.settled) notes.push('未静止(超时)')
    push(results, { i: s.i, ev: s.ev, ok: !why.length, why: why.join('；'), notes, ms: Date.now() - t0, diff: d.ratio, got: `${name}/${gotName}` })
  }
  return { name, results, shots }
}

