#!/usr/bin/env node
// 场景 runner（80 §5，appi.js 的精简重写）：直接驱动 host 调试口，不经 Appium。
//   node e2e/scenario.mjs <场景名|all> [包名...] [--rec] [--twice] [--corpus ../corpus] [--ignore y0-y1]
// --rec：同时用 host 的录制器（rec_start）录语料到 B，再拉到 <corpus>/trace/<设备>/<包>/<场景>/<批次>/。
// --twice：同一场景连跑两次，逐步标记处的画面必须一致（场景确定性）。
// 步骤原语：open waitStable mark(名) tap(选择器) scrollBy(px) back type(文本)；选择器 {id,text,desc} 或 {first:true}（主列表里第一个可点节点）。
import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { Host, sleep } from './lib/host.mjs'
import { diffPng } from './lib/png.mjs'
import { mainScrollable, calibrate, scrollBy, shotOf } from './lib/wheel.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
const args = process.argv.slice(2)
const flag = k => { const i = args.indexOf(k); if (i < 0) return false; args.splice(i, 1); return true }
const opt = (k, d) => { const i = args.indexOf(k); if (i < 0) return d; const v = args[i + 1]; args.splice(i, 2); return v }
const REC = flag('--rec'), TWICE = flag('--twice'), CORPUS = path.resolve(opt('--corpus', path.join(here, '../../corpus'))), PORT = +(opt('--host', 7701))
const IGN = []; for (let v; (v = opt('--ignore')); ) IGN.push(v.split('-').map(Number))
const [which = 'all', ...pkgArg] = args
const names = which === 'all' ? fs.readdirSync(path.join(here, 'scenarios')).map(f => f.replace(/\.mjs$/, '')) : [which]
const h = await new Host(PORT).connect()
const pkgs = pkgArg.length ? pkgArg : (await h.ok({ cmd: 'apps' })).apps
const dev = (await h.ok({ cmd: 'ping' })).dev.replace(/[^A-Za-z0-9]+/g, '-')
const S = { change: 0.005, settle_ms: 500, timeout_ms: 4000, ignore: IGN }
const probeF = pkg => { try { const j = JSON.parse(fs.readFileSync(path.join(here, '../doc', `probe-${dev}.json`), 'utf8')); return j.p1?.find(x => x.pkg === pkg)?.calibration?.F_eff } catch { return null } }

async function resolveFirst(c) {   // 主列表里第一个可点、非返回/搜索类的节点
  const t = (await h.ok({ cmd: 'tree', full: true })).tree
  const cands = []
  const walk = n => { if (n.click && !n.hidden && n.b[3] > n.b[1]) cands.push(n); (n.kids || []).forEach(walk) }
  t.windows.forEach(w => w.root && walk(w.root))
  const inList = c ? cands.filter(n => n.b[1] >= c.b[1] && n.b[3] <= c.b[3] && n.b[0] >= c.b[0]) : cands
  const pick = (inList.length ? inList : cands).sort((a, b) => a.b[1] - b.b[1])[0]
  return pick && { x: Math.round((pick.b[0] + pick.b[2]) / 2), y: Math.round((pick.b[1] + pick.b[3]) / 2), desc: pick.text || pick.desc || pick.id }
}

async function runOnce(sc, pkg) {
  const marks = {}, log = []
  const ctx = { c: null, F: null, final: null }
  const recDir = `/data/local/tmp/hj/rec/${pkg}-${sc.name}-${Date.now()}`
  for (const [op, a] of sc.steps) {
    const t0 = Date.now()
    if (op === 'open') {
      const r = await h.ok({ cmd: 'open', pkg })
      if (REC) await h.ok({ cmd: 'rec_start', dir: recDir })
      await h.ok({ cmd: 'mark', name: 'scenario:' + sc.name })
    } else if (op === 'waitStable') {
      // 静止 ≠ 画好了：冷启动的闪屏 / 黑屏阶段也会“变化后静止”。遇到空白帧就继续等下一次变化（最多 5 轮）。
      let r, n = 0
      do { r = await h.ok({ cmd: 'settle', settle: { ...S, settle_ms: 700, timeout_ms: 8000 }, stack: true }); n++ } while (isBlank(await shotOf(h)) && n < 5)
      log.push(`waitStable ${r.settle.settled ? '静止' : '超时未静止'} ${r.settle.tSettle}ms${n > 1 ? `（空白帧重等 ${n - 1} 轮）` : ''}`)
    } else if (op === 'mark') {
      await h.ok({ cmd: 'mark', name: a }); marks[a] = await shotOf(h)
    } else if (op === 'tap') {
      ctx.c ??= await mainScrollable(h)
      const p = a.first ? await resolveFirst(ctx.c) : null
      if (!p) { log.push('tap: 找不到目标'); return { log, marks, skip: '没有可点节点' } }
      await h.ok({ cmd: 'mark', name: `step:tap ${p.desc}` })
      await h.ok({ cmd: 'tap', x: p.x, y: p.y, settle: S, stack: true })
    } else if (op === 'scrollBy') {
      ctx.c ??= await mainScrollable(h)
      if (!ctx.c) return { log, marks, skip: '没有竖直滚动容器' }
      ctx.F ??= probeF(pkg) ?? await calibrate(h, ctx.c, S)
      if (!ctx.F) return { log, marks, skip: '容器不响应滚轮（需分步拖动回退）' }
      await h.ok({ cmd: 'mark', name: `step:scrollBy ${a}` })
      await scrollBy(h, ctx.c, ctx.F, a, S)
    } else if (op === 'back') { await h.ok({ cmd: 'mark', name: 'step:back' }); await h.ok({ cmd: 'back', settle: { ...S, settle_ms: 700, timeout_ms: 5000 }, stack: true }) }
    else if (op === 'type') await h.ok({ cmd: 'text', s: a, settle: S })
    else throw new Error('未知原语 ' + op)
    log.push(`${op}${a !== undefined ? ' ' + JSON.stringify(a) : ''} ${Date.now() - t0}ms`)
  }
  ctx.final = await shotOf(h)
  ctx.final.blank = isBlank(ctx.final)
  let trace = null
  if (REC) {
    await h.ok({ cmd: 'rec_stop' })
    trace = path.join(CORPUS, 'trace', dev, pkg, sc.name, new Date().toISOString().replace(/[-:T]/g, '').slice(0, 14))
    fs.mkdirSync(trace, { recursive: true })
    execFileSync('adb', [...(process.env.B ? ['-s', process.env.B] : []), 'pull', recDir + '/.', trace], { stdio: 'ignore' })
    execFileSync('adb', [...(process.env.B ? ['-s', process.env.B] : []), 'shell', 'rm -rf ' + recDir])
  }
  return { log, marks, final: ctx.final, trace }
}
const isBlank = img => { const d = img.data, f = d.readUInt32LE(0); for (let i = 0; i < d.length; i += 4 * 997) if (d.readUInt32LE(i) !== f) return false; return true }
const same = (m, a, b) => m[a] && m[b] && diffPng(m[a], m[b], IGN).ratio <= 0.001

let fail = 0
for (const n of names) {
  const sc = (await import(path.join(here, 'scenarios', n + '.mjs'))).default
  for (const pkg of pkgs) {
    const runs = []
    for (let k = 0; k < (TWICE ? 2 : 1); k++) {
      const r = await runOnce(sc, pkg).catch(e => ({ log: [], marks: {}, err: String(e.message || e) }))
      runs.push(r)
      const res = { ...r, same: (a, b) => same(r.marks, a, b) }
      const why = r.err ? '出错: ' + r.err : r.skip ? null : sc.check(res)
      const tag = r.err ? '✗' : r.skip ? '–' : why ? '✗' : '✓'
      console.log(`${tag} ${sc.name.padEnd(15)} ${pkg}${r.skip ? '  跳过：' + r.skip : ''}${why ? '  ' + why : ''}${r.trace ? '  → ' + path.relative(process.cwd(), r.trace) : ''}`)
      if (tag === '✗') { fail++; r.log.length && console.log('    ' + r.log.join('\n    ')) }
    }
    if (TWICE && runs.every(r => !r.skip && !r.err)) {
      const bad = Object.keys(runs[0].marks).filter(k => !same({ a: runs[0].marks[k], b: runs[1].marks[k] }, 'a', 'b'))
      if (bad.length) { fail++; console.log(`✗ ${sc.name} ${pkg} 两次录制在 [${bad.join(', ')}] 处画面不一致（场景不确定：动画 / 网络内容？）`) }
      else console.log(`✓ ${sc.name} ${pkg} 两次录制各标记处画面一致`)
    }
  }
}
await h.call({ cmd: 'close' }); h.close()
process.exit(fail ? 1 : 0)
