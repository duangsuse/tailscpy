#!/usr/bin/env node
// 场景 runner（80 §5，appi.js 的精简重写）：直接驱动 host 调试口，不经 Appium。
//   node e2e/scenario.mjs <场景名|all> [包名...] [--rec] [--twice] [--corpus ../corpus] [--ignore y0-y1]
// --rec：同时用 host 的录制器（rec_start）录语料到 B，再拉到 <corpus>/trace/<设备>/<包>/<场景>/<批次>/。
// --twice：同一场景连跑两次，逐步标记处的画面必须一致（场景确定性）。
// --live：实跑模式（80 §5）。runner 扮演 A：经调试口发 wire 消息（Open / Tap / Back / Text…），B 的编码 → 发送队列 → 虚拟 A 的 rd 解码整条链路都走，
//         性质在 **A 的画布**上检查（返回后 A 的画面等于进入前……），另查 A 画布与 B 当前帧逐像素一致（有损图片除外）、校验（Verify）零失败。不录语料。
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
const LIVE = flag('--live'), REC = flag('--rec'), TWICE = flag('--twice'), CORPUS = path.resolve(opt('--corpus', path.join(here, '../../corpus'))), PORT = +(opt('--host', 7701))
const IGN = []; for (let v; (v = opt('--ignore')); ) IGN.push(v.split('-').map(Number))
const [which = 'all', ...pkgArg] = args
const names = which === 'all' ? fs.readdirSync(path.join(here, 'scenarios')).map(f => f.replace(/\.mjs$/, '')) : [which]
const h = await new Host(PORT).connect()
const pkgs = pkgArg.length ? pkgArg : (await h.ok({ cmd: 'apps' })).apps
const dev = (await h.ok({ cmd: 'ping' })).dev.replace(/[^A-Za-z0-9]+/g, '-')
const S = { change: 0.005, settle_ms: 500, timeout_ms: 4000, ignore: IGN }
const probeF = pkg => { try { const j = JSON.parse(fs.readFileSync(path.join(here, '../doc', `probe-${dev}.json`), 'utf8')); return j.p1?.find(x => x.pkg === pkg)?.calibration?.F_eff } catch { return null } }

async function resolveFirst(c) {   // 主列表里第一个可点、非返回/搜索类的节点
  const cands = []
  const walk = n => { if (n.click && !n.hidden && n.b[3] > n.b[1]) cands.push(n); (n.kids || []).forEach(walk) }
  for (let i = 0; i < 12 && !cands.length; i++) {   // A11y 树总是晚于画面：刚打开时窗口列表可能还是空的，最多等 ~3s
    if (i) await sleep(250)
    const t = (await h.ok({ cmd: 'tree', full: true })).tree
    t.windows.forEach(w => w.root && walk(w.root))
  }
  const inList = c ? cands.filter(n => n.b[1] >= c.b[1] && n.b[3] <= c.b[3] && n.b[0] >= c.b[0]) : cands
  const pick = (inList.length ? inList : cands).sort((a, b) => a.b[1] - b.b[1])[0]
  return pick && { x: Math.round((pick.b[0] + pick.b[2]) / 2), y: Math.round((pick.b[1] + pick.b[3]) / 2), desc: pick.text || pick.desc || pick.id }
}

/** 按 desc / text / id 后缀找节点（不要求可点：有的按钮只是 ImageView 外面包了可点父节点），取中心 */
async function resolveBy(sel) {
  for (let i = 0; i < 12; i++) {
    if (i) await sleep(250)
    const t = (await h.ok({ cmd: 'tree', full: true })).tree; let hit = null
    const walk = n => { if (!hit && !n.hidden && n.b[3] > n.b[1] && ((sel.desc && n.desc === sel.desc) || (sel.text && n.text === sel.text) || (sel.id && n.id?.endsWith(sel.id)))) hit = n; (n.kids || []).forEach(walk) }
    t.windows.forEach(w => w.root && walk(w.root))
    if (hit) return { x: Math.round((hit.b[0] + hit.b[2]) / 2), y: Math.round((hit.b[1] + hit.b[3]) / 2), desc: hit.desc || hit.text || hit.id }
  }
  return null
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


// ───────── 实跑模式 ─────────
const wire = msg => h.ok({ cmd: 'wire', msg })
const vaState = async rows => (await h.ok({ cmd: 'va_state', rows })).va
async function vaIdle(timeout = 12000) {   // 链路空闲（连续 3 次）
  const t0 = Date.now(); let ok = 0
  while (Date.now() - t0 < timeout) { const v = await vaState(false); ok = v.idle ? ok + 1 : 0; if (ok >= 2) return v; await sleep(100) }
  return await vaState(false)
}
/** 扮演 A 的视口：不断报同一个目标，直到采集头到位或到边（B 在换页 / 加载时会丢掉旧目标，A 本来就会重报） */
async function moveHead(target, timeoutMs = 9000) {
  const t0 = Date.now()
  while (Date.now() - t0 < timeoutMs) {
    await wire({ View: { page: 1, region: 1, y: target, vy: 0 } }); await sleep(250)
    const v = await vaState(false); if (!v.scroll) break
    if (Math.abs(v.scroll.head - target) <= 2) break
    if ((target > v.scroll.head && v.scroll.at_bottom) || (target < v.scroll.head && v.scroll.at_top)) break
  }
  return await vaIdle()
}
async function runLive(sc, pkg) {
  const marks = {}, log = []; let first = null
  await h.ok({ cmd: 'policy', allow: sc.policy ?? ['[popup]'] }); await h.ok({ cmd: 'param', key: 'verify', val: 1 })   // 通用场景会点进任意子页（含设置的搜索），所以放行临时窗口；off-whitelist 之类的场景用 sc.policy 覆盖
  await h.ok({ cmd: 'va_attach', on: true })
  const snap = async () => { let v = await vaState(true); for (let i = 0; i < 20 && !(v.idle && !v.diff_px); i++) { await sleep(80); v = await vaState(true) }   // 光标闪烁等会让 B 的帧在两次读取之间变掉：同一次读取里空闲且一致才算数
    return { rows: v.rows, blank: v.blank, diff_px: v.diff_px, verify_fail: v.verify_fail, rx: v.rx_bytes, err: v.err, comparable: v.comparable } }
  let c = null
  for (const [op, a] of sc.steps) {
    const t0 = Date.now()
    if (op === 'open') {
      await wire({ Open: { pkg, geom: null } })
      const t = Date.now(); while (Date.now() - t < 20000 && !((await vaState(false)).txs > 0)) await sleep(150)
    } else if (op === 'waitStable') {
      let v, n = 0
      do { v = await vaIdle(); n++; if (v.blank && n < 5) await sleep(800) } while (v.blank && n < 5)
      if (!first) first = { bytes: v.rx_bytes, ms: Date.now() - t0 }
      log.push(`waitStable ${v.idle ? '空闲' : '超时未空闲'} rx=${v.rx_bytes}B`)
    } else if (op === 'mark') { await vaIdle(); marks[a] = await snap() }
    else if (op === 'tap') {
      c ??= await mainScrollable(h)
      const p = a.first ? await resolveFirst(c) : await resolveBy(a)
      if (!p) return { log, marks, skip: '没有可点节点' }
      await wire({ Tap: { page: 1, at: { region: 0, x: p.x, y: p.y } } })
    } else if (op === 'back') { await wire('Back'); await sleep(300) }
    else if (op === 'type') await wire({ Text: { s: a } })
    else if (op === 'scrollBy') {
      // A 的本地滚动：视口（画布 y）往目标走，B 的采集头去追。每步 ≤ 700 px，每步等链路空闲；到边（采集头不再动）就停
      let v = await vaIdle()
      for (let i = 0; i < 20 && !v.scroll; i++) { await sleep(250); v = await vaIdle() }   // A11y 提案总是晚于画面
      if (!v.scroll) return { log, marks, skip: '没有 Scroll 区域（A11y 没给出可滚动节点）' }
      let left = Math.abs(a), moved = 0; const dir = a >= 0 ? 1 : -1
      while (left > 0) {
        const n = Math.min(700, left), before = v.scroll.head
        v = await moveHead(before + dir * n); const dy = v.scroll.head - before
        if (Math.abs(dy) < n * 0.5) break   // 到边
        left -= n; moved += dy
      }
      log.push(`scrollBy ${a} 实际 ${moved}px`)
    }
    else throw new Error('未知原语 ' + op)
    log.push(`${op}${a !== undefined ? ' ' + JSON.stringify(a) : ''} ${Date.now() - t0}ms`)
  }
  await vaIdle()
  const fin = await snap(); fin.first = first
  await wire('Close'); await h.ok({ cmd: 'va_attach', on: false })
  return { log, marks, final: fin, live: true }
}
const sameLive = (m, a, b) => { const x = m[a]?.rows, y = m[b]?.rows; if (!x || !y || x.length !== y.length) return false; return x.filter((v, i) => v !== y[i]).length <= x.length * 0.002 }
// A 与 B、校验
const liveProps = r => {
  const bad = []
  for (const [k, m] of Object.entries({ ...r.marks, final: r.final })) {
    if (m.comparable && m.diff_px > 0) bad.push(`${k}: A 画布与 B 帧有 ${m.diff_px} 个像素不同`)
    if (m.verify_fail > 0) bad.push(`${k}: Verify 失败 ${m.verify_fail} 次`)
    if (m.err) bad.push(`${k}: 解码出错 ${m.err}`)
  }
  return bad.slice(0, 3).join('；')
}

let fail = 0
for (const n of names) {
  const sc = (await import(path.join(here, 'scenarios', n + '.mjs'))).default
  for (const pkg of (sc.pkgs && !pkgArg.length ? sc.pkgs.filter(x => pkgs.includes(x)) : pkgs)) {
    const runs = []
    for (let k = 0; k < (TWICE ? 2 : 1); k++) {
      const r = await (LIVE ? runLive(sc, pkg) : runOnce(sc, pkg)).catch(e => ({ log: [], marks: {}, err: String(e.message || e) }))
      runs.push(r)
      const res = { ...r, same: (a, b) => LIVE ? sameLive(r.marks, a, b) : same(r.marks, a, b) }
      const gone = LIVE && r.err && /没有会话/.test(r.err)
      const why = gone ? null : r.err ? '出错: ' + r.err : r.skip ? null : [sc.check(res), LIVE ? liveProps(r) : null].filter(Boolean).join('；') || null
      if (LIVE && r.err && /没有会话/.test(r.err)) { r.err = null; r.skip = '该步让 App 退出了（根页 Back → AppGone，会话按策略结束），场景不适用' }
      const tag = r.err ? '✗' : r.skip ? '–' : why ? '✗' : '✓'
      console.log(`${tag} ${sc.name.padEnd(15)} ${pkg}${r.skip ? '  跳过：' + r.skip : ''}${why ? '  ' + why : ''}${r.trace ? '  → ' + path.relative(process.cwd(), r.trace) : ''}${r.live && r.final?.first ? `  首屏 ${(r.final.first.bytes / 1000).toFixed(0)} KB / ${r.final.first.ms} ms` : ''}`)
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
