#!/usr/bin/env node
// 语料回放工具（80 §3）：
//   node e2e/replay.mjs list   <trace目录>                 事件时间线摘要（类型计数 + 步骤 / 标记 / 栈）
//   node e2e/replay.mjs export <trace目录> <帧号> <out.png> 把 seq ≤ 帧号的最近一帧导出为 PNG
//   node e2e/replay.mjs pull   <B上目录> <本机目录>          adb pull 录制（用环境变量 B 选设备）
import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { openTrace } from './lib/trace.mjs'
import { encodePng } from './lib/png.mjs'

const [cmd, a, b, c] = process.argv.slice(2)
if (cmd === 'pull') {
  const s = process.env.B; fs.mkdirSync(b, { recursive: true })
  execFileSync('adb', [...(s ? ['-s', s] : []), 'pull', a + '/.', b], { stdio: 'inherit' })
} else if (cmd === 'list') {
  const t = openTrace(a), n = {}
  for (const e of t.events) n[e.type] = (n[e.type] || 0) + 1
  console.log(`${t.manifest.pkg} ${t.manifest.w}x${t.manifest.h}@${t.manifest.dpi} ${t.manifest.device} sdk${t.manifest.sdk}`, n, `帧记录 ${t.idx.length}`)
  for (const e of t.events) if (['step', 'mark', 'stack', 'a11y_tree'].includes(e.type)) console.log(String(e.t).padStart(7), e.type, e.name || e.act || e.file || `${e.depth} ${e.top}`)
  // 时间顺序检查：t 单调不减
  let bad = 0; for (let i = 1; i < t.events.length; i++) if (t.events[i].t < t.events[i - 1].t) bad++
  console.log(bad ? `⚠ ${bad} 处时间戳倒退` : '时间戳单调 ✓')
} else if (cmd === 'export') {
  const t = openTrace(a), f = t.frame(+b)
  fs.writeFileSync(c, encodePng(f)); console.log(`seq ${f.seq} (t=${f.t}ms) → ${c}`)
} else { console.error(fs.readFileSync(new URL(import.meta.url), 'utf8').split('\n').slice(1, 6).join('\n')); process.exit(2) }
