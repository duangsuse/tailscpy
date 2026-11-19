#!/usr/bin/env node
// 回放人工录制的 jsonl 并逐步比对截图：
//   node e2e/run.mjs [cases/<包名>.jsonl ...]  [--tol 0.001] [--repeat 2] [--out e2e/out] [--host 7701]
// 不给文件则跑 e2e/cases/*.jsonl。每步：按录制的等待参数注入 → 等“变化阈值 + 静止” → 截图 → 与录制的 PNG 比对（不同像素占比 ≤ tol）；
// 另查：该步“有无变化”、栈深与录制一致。--repeat N：同一 case 连跑 N 次，并比较各次之间的截图（确定性）。
// 录制的 tap 带选择器：回放时在点前的节点树里重新找到那个节点，布局没变就用原坐标，变了就按节点相对位置换算。
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Host } from './lib/host.mjs'
import { diffPng } from './lib/png.mjs'
import { loadCase, runCase } from './lib/replay.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
const args = process.argv.slice(2)
const opt = (k, d) => { const i = args.indexOf(k); if (i < 0) return d; const v = args[i + 1]; args.splice(i, 2); return v }
const TOL = +opt('--tol', 0.001), REPEAT = +opt('--repeat', 1), OUT = path.resolve(opt('--out', path.join(here, 'out'))), HOSTP = +opt('--host', 7701)
const files = args.length ? args.map(f => path.resolve(f)) : fs.readdirSync(path.join(here, 'cases')).filter(f => f.endsWith('.jsonl')).map(f => path.join(here, 'cases', f))
if (!files.length) { console.error('没有 case：先用 node e2e/recorder.mjs 录制'); process.exit(2) }

const h = await new Host(HOSTP).connect()
let fail = 0
for (const f of files) {
  const c = loadCase(f)
  const runs = []
  for (let k = 1; k <= REPEAT; k++) {
    console.log(`\n== ${c.meta.pkg}  (${c.steps.length} 步${REPEAT > 1 ? `，第 ${k}/${REPEAT} 次` : ''})`)
    const r = await runCase(h, c, { tol: TOL, outDir: path.join(OUT, `run${k}`) }); runs.push(r)
    for (const x of r.results) { console.log(`  ${x.ok ? '✓' : '✗'} #${x.i} ${x.ev || ''} ${x.ok ? `diff ${(x.diff * 100).toFixed(4)}% ${x.ms}ms` : x.why}${x.notes?.length ? '  [' + x.notes.join('；') + ']' : ''}`); if (!x.ok) fail++ }
  }
  if (REPEAT > 1) {   // 确定性：各次之间逐步比对
    for (let k = 1; k < REPEAT; k++) for (let i = 0; i < Math.min(runs[0].shots.length, runs[k].shots.length); i++) {
      if (c.steps[i]?.ev === 'open') continue   // open 只等固定时间，不参与次间一致性
      const d = diffPng(runs[0].shots[i], runs[k].shots[i], c.steps[i]?.wait?.ignore || [])
      if (d.ratio > TOL) { fail++; console.log(`  ✗ 第 1 次与第 ${k + 1} 次 #${i} 不一致 ${d.pixels}px bbox ${JSON.stringify(d.bbox)}（场景不确定：有动画 / 网络内容？）`) }
    }
  }
}
await h.call({ cmd: 'close' }); h.close()
console.log(fail ? `\n失败 ${fail} 项（逐步截图在 ${OUT}）` : '\n全部通过')
process.exit(fail ? 1 : 0)
