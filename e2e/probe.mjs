#!/usr/bin/env node
// M0 探针（60 §9）：node e2e/probe.mjs <p1|p2|p4|p7|p8|jpeg|all> [pkg...]   结果 JSON 打到 stdout，并写入 doc/probe-<设备>.md 的对应小节。
// 每个探针 = host 调试口命令的组合（host 侧只提供原语：wheel / tree / text / scroll_to / jpeg_bench …），判定逻辑在这里。
import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { Host, sleep } from './lib/host.mjs'
import { decodePng } from './lib/png.mjs'
import { estimateShift } from './lib/shift.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
const [name = 'all', ...pkgs] = process.argv.slice(2)
const h = await new Host(+(process.env.PORT || 7701)).connect()
const shot = async () => decodePng(Buffer.from((await h.ok({ cmd: 'frame', fmt: 'png' })).data, 'base64'))
const S = { change: 0.005, settle_ms: 500, timeout_ms: 4000 }
const apps = pkgs.length ? pkgs : (await h.ok({ cmd: 'apps' })).apps
const out = {}
const dev = ((await h.call({ cmd: 'ping' })).dev || 'device').replace(/[^A-Za-z0-9]+/g, '-')   // 先取：p8 会杀掉 host

/** 取虚拟屏上最大的可滚动节点（只看竖直可滚的）。 */
async function scrollable() {
  const t = (await h.ok({ cmd: 'tree', full: true })).tree
  let best = null
  const walk = n => {
    if (n.scroll && !n.hidden && !/ViewPager|Horizontal|Pager/i.test((n.cls || "") + (n.id || ""))) { const [l, tp, r, b] = n.b, a = (r - l) * (b - tp); if (!best || a > best.a) best = { n, a } }
    for (const k of n.kids || []) walk(k)
  }
  for (const w of t.windows) if (w.root) walk(w.root)
  if (!best) return null
  const [l, tp, r, b] = best.n.b
  return { id: best.n.id, cls: best.n.cls, b: best.n.b, x: Math.round((l + r) / 2) - 40, y: Math.round((tp + b) / 2), dirs: best.n.scrollDirs, rows: best.n.coll?.rows }
}
const openApp = async pkg => {
  const r = await h.call({ cmd: 'open', pkg }); if (!r.ok) return r
  await h.ok({ cmd: 'settle', settle: { ...S, settle_ms: 700, timeout_ms: 6000 } }); return r
}

// ── P1 滚轮注入与远跳 ──
async function p1(pkg) {
  const o = await openApp(pkg); if (!o.ok) return { pkg, error: o.err }
  const c = await scrollable(); if (!c) return { pkg, verdict: '无竖直滚动容器', fallback: '—' }
  const res = { pkg, container: `${c.cls.split('.').pop()} ${c.id || ''}`, rows: c.rows }
  // 标定：一次 v=-1 的格，量实际位移 → 有效因子 F_eff（像素/格）。每个 App 的容器都可能和 ViewConfiguration 报的不同。
  const wheel = async v => {
    const a = await shot()
    await h.ok({ cmd: 'wheel', x: c.x, y: c.y, v, settle: S })
    await sleep(150)
    const b = await shot(); await sleep(1200); const b2 = await shot()
    const s1 = estimateShift(a, b), s2 = estimateShift(a, b2)
    return { dy: s1.dy, dyLate: s2.dy, votes: s1.votes }
  }
  const cal = await wheel(-1)
  if (Math.abs(cal.dy) < 2) { res.verdict = '不支持滚轮（v=-1 无位移）'; res.fallback = '分步拖动'; return res }
  const F = Math.abs(cal.dy) / 1
  res.calibration = { dy: cal.dy, F_eff: F }
  // 验证精确公式：注入 N 像素 → v = -(N+0.5)/F_eff，实际位移应 = N
  const trials = []
  for (const N of [1, 5, 37, 120, 333]) {
    const r = await wheel(-(N + 0.5) / F)
    trials.push({ N, dy: r.dy, dyLate: r.dyLate, exact: r.dy === N, settled: r.dy === r.dyLate })
  }
  res.trials = trials
  const exact = trials.filter(t => t.exact).length, still = trials.every(t => t.settled)
  res.verdict = exact === trials.length ? (still ? '精确' : '精确位移但之后仍有移动(惯性/动画)') : still ? `近似（${exact}/${trials.length} 精确）` : '有动画 / 惯性'
  res.fallback = exact === trials.length && still ? '无需' : '有动画或不精确的容器用分步拖动'
  // 到边不回弹：连续大步滚到底再多滚几格，位移应为 0 且无回弹
  for (let i = 0; i < 25; i++) await h.ok({ cmd: 'wheel', x: c.x, y: c.y, v: -4, settle: { ...S, settle_ms: 150, timeout_ms: 800 } })
  const e1 = await shot(); await h.ok({ cmd: 'wheel', x: c.x, y: c.y, v: -2, settle: { ...S, settle_ms: 300, timeout_ms: 1500 } }); await sleep(800); const e2 = await shot()
  res.edge = { extraShift: estimateShift(e1, e2).dy, bounce: estimateShift(e1, e2).dy !== 0 ? '有位移（回弹/未到底）' : '无' }
  // 远跳：RecyclerView 的 ACTION_SCROLL_TO_POSITION
  if (c.rows > 20) {
    const before = await shot()
    const r = await h.ok({ cmd: 'scroll_to', row: Math.floor(c.rows / 2), settle: S })
    const after = await shot(), t = (await h.ok({ cmd: 'tree', full: true })).tree
    const rowsSeen = []; const walk = n => { if (n.item) rowsSeen.push(n.item.row); (n.kids || []).forEach(walk) }; t.windows.forEach(w => w.root && walk(w.root))
    res.scrollToPosition = { msg: r.msg, visibleRows: [Math.min(...rowsSeen), Math.max(...rowsSeen)], target: Math.floor(c.rows / 2), changed: estimateShift(before, after).votes < 200 }
  }
  return res
}

// ── P2 虚拟屏 A11y 与 IPC 耗时 ──
async function p2() {
  const o = await openApp('com.android.settings'); if (!o.ok) return { error: o.err }
  const ts = [], seqGap = []
  let nodes = 0, hasRowIndex = false, hasColl = false
  for (let i = 0; i < 15; i++) {
    const s0 = (await h.ok({ cmd: 'status' })).seq
    const t = (await h.ok({ cmd: 'tree', full: true })); ts.push(t.tree.ipcMs)
    const s1 = (await h.ok({ cmd: 'status' })).seq; seqGap.push(s1 - s0)
    if (!i) { const walk = n => { nodes++; if (n.item) hasRowIndex = true; if (n.coll) hasColl = true; (n.kids || []).forEach(walk) }; t.tree.windows.forEach(w => w.root && walk(w.root)) }
  }
  ts.sort((a, b) => a - b)
  return { windowOnVirtualDisplay: nodes > 0, nodes, collectionItemInfo_rowIndex: hasRowIndex, collectionInfo: hasColl,
    ipcMs: { p50: ts[7], p95: ts[13], max: ts[14] }, framesDuringDump_p50: seqGap.sort((a, b) => a - b)[7],
    verdict: nodes > 0 && hasRowIndex ? '通过' : '不通过', fallback: nodes > 0 && hasRowIndex ? '无需' : '该设备只做整屏 Fixed（没有滚动优化）' }
}

// ── P4 输入法策略与文本 ──
async function p4() {
  const o = await openApp('com.android.settings'); if (!o.ok) return { error: o.err }
  await h.ok({ cmd: 'tap', x: 540, y: 588, settle: { ...S, settle_ms: 800, timeout_ms: 5000 } })   // 搜索栏 → 搜索页（有输入框）
  const ime = () => execFileSync('adb', [...(process.env.B ? ['-s', process.env.B] : []), 'shell', 'dumpsys input_method | grep -E "mInputShown|mWindowVisible|isShowing" | head -5'], { encoding: 'utf8' }).trim()
  const a = await h.ok({ cmd: 'text', s: 'wifi', settle: S }), b = await h.ok({ cmd: 'text', s: '你好', settle: S })
  await sleep(500)
  const t = (await h.ok({ cmd: 'tree', full: false })).tree
  const texts = []; const walk = n => { if (n.edit) texts.push(n.text); (n.kids || []).forEach(walk) }; t.windows.forEach(w => w.root && walk(w.root))
  const f = await shot(), bottom = f.h - 1
  return { ascii: a.via, chinese: b.via, editTextContent: texts, imeDump: ime(), verdict: texts.some(x => x?.includes('你好')) ? '文本可输入' : '不通过', fallback: texts.some(x => x?.includes('你好')) ? '无需' : '文本改走剪贴板粘贴', note: '输入法窗口是否出现在虚拟屏/物理屏见 imeDump 与截图（截图底部无键盘）' , bottomPixel: [...f.data.subarray((bottom * f.w) * 4, (bottom * f.w) * 4 + 4)] }
}

// ── P7 熄屏 ──
async function p7() {
  const adb = (...a) => execFileSync('adb', [...(process.env.B ? ['-s', process.env.B] : []), ...a], { encoding: 'utf8' }).trim()
  const o = await openApp('com.android.settings'); if (!o.ok) return { error: o.err }
  // 先让物理屏真的熄灭（KEYCODE_SLEEP(223)；轮询确认），没熄灭就不下结论；无论如何结束时唤醒
  const wf = () => adb('shell', 'dumpsys power 2>/dev/null | grep -m1 mWakefulness=').trim()
  const waitFor = async (re, ms) => { const t0 = Date.now(); let s = wf(); while (!re.test(s) && Date.now() - t0 < ms) { await sleep(300); s = wf() } return s }
  try {
    adb('shell', 'input keyevent 223')
    const st = await waitFor(/Asleep|Dozing/, 6000)
    if (!/Asleep|Dozing/.test(st)) return { power: st, stayOn: adb('shell', 'dumpsys power 2>/dev/null | grep -m1 mStayOn=').trim(), verdict: '未判定：物理屏没能熄灭', fallback: '—', note: 'mStayOn=true 表示设备正在充电且开着“充电时保持唤醒”，熄屏键被系统忽略；不得改表外设置，需拔电或用户关掉该开发者选项后再测' }
    await sleep(1500)
    const r = await h.ok({ cmd: 'swipe', x1: 540, y1: 2000, x2: 540, y2: 1200, ms: 300, settle: S })
    return { power: st, framesAfterInjectWhileScreenOff: r.settle, verdict: r.settle.changed ? '通过' : '不通过', fallback: r.settle.changed ? '无需（host 持有部分唤醒锁）' : '会话期间保持亮屏并调低亮度' }
  } finally { adb('shell', 'input keyevent 224'); await waitFor(/Awake/, 4000) }
}

// ── P8 设置还原（含看门狗）：kill -9 host，2 秒内不重启 host，设置回到原值 ──
async function p8() {
  const adb = (...a) => execFileSync('adb', [...(process.env.B ? ['-s', process.env.B] : []), ...a], { encoding: 'utf8' }).trim()
  const keys = ['window_animation_scale', 'transition_animation_scale', 'animator_duration_scale', 'disable_window_blurs']
  const get = () => keys.map(k => adb('shell', `settings get global ${k}`))
  const o = await openApp('com.android.settings'); if (!o.ok) return { error: o.err }
  const orig = JSON.parse(adb('shell', 'cat /data/local/tmp/hj/restore.json')), applied = get()
  const t0 = Date.now(); adb('shell', `kill -9 $(cat /data/local/tmp/hj/pid.${process.env.PORT || 7701})`)
  let back = null
  while (Date.now() - t0 < 4000) { const v = get(); if (keys.every((k, i) => v[i] === orig[k] || (orig[k] === null && v[i] === 'null'))) { back = Date.now() - t0; break } await sleep(100) }
  return { orig, appliedDuringSession: applied, restoredAfterMs: back, verdict: back !== null && back <= 2000 ? '通过' : '不通过', note: '含 adb 往返开销' }
}

async function jpeg() {
  const o = await openApp('com.android.settings'); if (!o.ok) return { error: o.err }
  const q = [50, 70, 90]; const r = {}
  for (const x of q) r['q' + x] = await h.ok({ cmd: 'jpeg_bench', q: x, n: 7 })
  return { note: 'B 端 Bitmap.compress(JPEG)，1080x2640 整屏，中位数；设置首页（文字为主）', results: Object.fromEntries(Object.entries(r).map(([k, v]) => [k, { msPerMP: +v.msPerMP.toFixed(1), bytes: v.bytes, medianMs: +v.medianMs.toFixed(1) }])) }
}

const run = { p1: async () => { const r = []; for (const a of apps) { console.error('P1', a); r.push(await p1(a).catch(e => ({ pkg: a, error: String(e) }))) } return r }, p2, p4, p7, p8, jpeg }
const todo = name === 'all' ? ['p2', 'p4', 'jpeg', 'p1', 'p7', 'p8'] : [name]
for (const n of todo) { console.error('==', n); out[n] = await run[n](); console.log(JSON.stringify({ [n]: out[n] })) }
await h.call({ cmd: 'close' }).catch(() => {}); h.close()
const f = path.join(here, '..', 'doc', `probe-${dev}.json`)
let all = {}; try { all = JSON.parse(fs.readFileSync(f, 'utf8')) } catch {}
if (out.p1 && all.p1) { const m = new Map(all.p1.map(x => [x.pkg, x])); for (const x of out.p1) m.set(x.pkg, x); out.p1 = [...m.values()] }   // P1 按包名合并（可只重测个别 App）
fs.writeFileSync(f, JSON.stringify({ ...all, ...out }, null, 1)); console.error('已写', f)
