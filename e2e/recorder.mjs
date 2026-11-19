#!/usr/bin/env node
// 人工录制入口：本机起一个网页，显示 B 的虚拟屏；你用鼠标操作，每个 App 录一个 jsonl + 截图，由本服务直接落盘，e2e/run.mjs 直接回放。
//   node e2e/recorder.mjs [--port 8800] [--host 7701] [--cases e2e/cases]
// 浏览器打开 http://localhost:8800/ → 选 App → 打开 → 左键点按/长按/拖动，右键或 Esc 返回。
// 每个事件注入后等“画面变化超过阈值 → 再静止”（默认），之后截图（格式见 e2e/README.md）。
import http from 'node:http'
import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { Host, sleep } from './lib/host.mjs'
import { classify, inject } from './lib/steps.mjs'
import { loadCase, runCase } from './lib/replay.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d }
const PORT = +arg('--port', 8800), HOST_PORT = +arg('--host', 7701)
const OPEN_WAIT = { change: 0.005, settle_ms: 700, timeout_ms: 700 }   // settle_ms = timeout_ms → 恰好等 700ms
const CASES = path.resolve(arg('--cases', path.join(here, 'cases')))

// ── 保存目录：启动时就验证（建得出来、写得进去、读得回来），不对就直接退出，不等录到一半才发现 ──
function checkCasesDir() {
  fs.mkdirSync(CASES, { recursive: true })
  const probe = path.join(CASES, '.write-test'), mark = String(Date.now())
  fs.writeFileSync(probe, mark)
  const ok = fs.readFileSync(probe, 'utf8') === mark
  fs.rmSync(probe)
  if (!ok) throw new Error('写入后读回不一致')
}
try { checkCasesDir() } catch (e) { console.error(`保存目录不可用：${CASES}\n  ${e.message}`); process.exit(1) }

// ── host：连不上就用仓库自己的 host-adb/run.sh 拉起一次，再不行给出明确原因 ──
const act = new Host(HOST_PORT)    // 动作连接：open / 注入 / settle，串行
const side = new Host(HOST_PORT)   // 旁路连接：取节点树（点击前查目标节点）
const prev = new Host(HOST_PORT)   // 预览连接：取预览帧
async function ensureHost() {
  try { return await act.ok({ cmd: 'ping' }) } catch (e) {
    console.log('host 没在运行，尝试 host-adb/run.sh …')
    try { execFileSync(path.join(here, '../host-adb/run.sh'), [String(HOST_PORT)], { stdio: 'inherit', env: process.env }) }
    catch { throw new Error(`host 起不来：${e.message}（检查 adb devices / 设置 B=序列号 后手动运行 host-adb/run.sh）`) }
    return act.ok({ cmd: 'ping' })
  }
}

let session = null    // {pkg, w, h, dpi}
let g = null          // 进行中的手势 {x,y,tree:Promise}
const OUTWEB = path.join(here, 'out', 'web')   // 网页回放的逐步截图
let job = null        // 回放任务：{pkg, tol, results:[], running, error}
let rec = null        // 正在录的 App：{pkg, lines:[meta, step…], n}

const json = (res, o, code = 200) => { res.writeHead(code, { 'content-type': 'application/json' }); res.end(JSON.stringify(o)) }
const body = req => new Promise(r => { let b = ''; req.on('data', d => b += d); req.on('end', () => r(b ? JSON.parse(b) : {})) })

/** 写文件并读回校验大小；返回绝对路径。rel 必须落在 CASES 之内。 */
function save(rel, data) {
  const p = path.resolve(CASES, rel)
  if (!p.startsWith(CASES + path.sep)) throw new Error('路径越界：' + rel)
  fs.mkdirSync(path.dirname(p), { recursive: true })
  fs.writeFileSync(p, data)
  const want = Buffer.byteLength(data), got = fs.statSync(p).size
  if (got !== want) throw new Error(`写入校验失败 ${p}：期望 ${want} 字节，实际 ${got}`)
  return p
}
const DRAFT = '.draft'   // 录制中先写草稿；点“保存此重录”才落成正式 case
/** 用例名：不含路径分隔符、不以 . 开头（否则会写到 CASES 之外或藏进草稿目录）。可以是中文。 */
const validName = n => typeof n === 'string' && n.length > 0 && n.length <= 80 && !/[\/\\]/.test(n) && !n.startsWith('.')
const saveJsonl = () => save(`${DRAFT}/${rec.pkg}.jsonl`, rec.lines.map(l => JSON.stringify(l)).join('\n') + '\n')
/** 记一步：写 PNG 与 jsonl，返回给网页的描述（截图走 /shots/ 取，不再把 base64 送回去）。 */
function addStep(step, wait) {
  const i = rec.n++, name = `${rec.pkg}/${String(i).padStart(4, '0')}.png`
  const { shot, ...rest } = step
  const file = save(`${DRAFT}/${name}`, Buffer.from(shot, 'base64'))
  rec.lines.push({ type: 'step', i, ...rest, wait, shot: name })
  const jsonl = saveJsonl()
  return { ...rest, i, wait, shot: `/shots/${DRAFT}/${name}`, file, jsonl }
}

// ── 预览：SSE，有订阅者时轮询 host 取缩放 JPEG（只在帧序号变化时才有数据）──
const subs = new Set()
let pumping = false
let since = -1   // 预览已推送到的帧序号；换 App / 回放时 host 的帧序号从头计，必须清零，否则一直被当作“没有新帧”
async function pump() {
  if (pumping) return; pumping = true
  while (subs.size) {
    try {
      if (!session) { await sleep(200); continue }
      const r = await prev.call({ cmd: 'frame', fmt: 'jpeg', scale: 0.5, q: 65, since })
      if (!r.ok) { await sleep(200); continue }
      if (r.same) { await sleep(25); continue }
      since = r.seq
      const msg = `data: ${JSON.stringify({ seq: r.seq, w: r.w, h: r.h, img: r.data })}\n\n`
      for (const s of subs) s.write(msg)
    } catch (e) { await sleep(300) }
  }
  pumping = false
}

// ── 选择器：点击前的节点树里，包含点击点的最深的“可点击 / 有标识”节点 ──
function pickSel(tree, x, y) {
  let best = null
  const walk = (n, d) => {
    const [l, t, r, b] = n.b
    if (x < l || x >= r || y < t || y >= b || n.hidden) return
    if (n.click || n.id || n.text || n.desc) best = { n, d }
    for (const k of n.kids || []) walk(k, d + 1)
  }
  // 窗口按 layer 从高到低，第一个包含该点且有命中的窗口为准
  const ws = [...tree.windows].sort((a, b) => b.layer - a.layer)
  for (const w of ws) { if (w.root) walk(w.root, 0); if (best) break }
  if (!best) return null
  const n = best.n
  const o = { cls: n.cls, b: n.b }
  if (n.id) o.id = n.id
  if (n.text) o.text = n.text
  if (n.desc) o.desc = n.desc
  if (n.item) o.row = n.item.row
  return o
}

const routes = {
  'GET /api/info': async (req, res) => {
    const existing = fs.readdirSync(CASES).filter(f => f.endsWith('.jsonl')).map(f => f.slice(0, -6))
    let host = null; try { const r = await ensureHost(); host = { dev: r.dev, rd: r.rd } } catch (e) { host = { error: e.message } }
    json(res, { ok: true, cases: CASES, writable: true, existing, host })
  },
  'GET /api/apps': async (req, res) => { await ensureHost(); json(res, await act.call({ cmd: 'apps' })) },
  'POST /api/open': async (req, res) => {
    const { pkg, wait } = await body(req)
    if (!/^[\w.]+$/.test(pkg)) return json(res, { ok: false, err: '包名不合法' })
    const info = await ensureHost()
    const r = await act.call({ cmd: 'open', pkg })
    if (!r.ok) return json(res, r)
    session = { pkg, w: r.w, h: r.h, dpi: r.dpi }; since = -1
    // open 步骤不等静止帧：固定等 OPEN_WAIT 就截图（首屏常有闪屏 / 走马灯，等静止没有意义）
    const s = await act.call({ cmd: 'settle', settle: OPEN_WAIT, stack: true, shot: true })
    fs.rmSync(path.join(CASES, DRAFT, pkg), { recursive: true, force: true })
    rec = { pkg, n: 0, lines: [{ v: 1, type: 'meta', pkg, device: info.dev, sdk: info.sdk, w: r.w, h: r.h, dpi: r.dpi, created: new Date().toISOString(), wait }] }
    const first = addStep({ ev: 'open', res: s.settle, stack: s.stack, shot: s.shot }, { ...OPEN_WAIT, ignore: [] })
    json(res, { ok: true, session, first })
  },
  // 回放：用服务端自己的 host 连接跑 lib/replay.mjs（和 e2e/run.mjs 同一份），逐步结果由网页轮询 GET /api/replay
  'POST /api/replay': async (req, res) => {
    const { pkg, tol } = await body(req)
    if (job?.running) return json(res, { ok: false, err: '上一个回放还在跑' })
    if (!validName(pkg)) return json(res, { ok: false, err: '用例名不合法' })
    const file = path.join(CASES, pkg + '.jsonl')
    if (!fs.existsSync(file)) return json(res, { ok: false, err: '没有这个 case：' + file })
    await ensureHost()
    const c = loadCase(file)
    rec = null; g = null; session = { pkg, w: c.meta.w, h: c.meta.h, dpi: c.meta.dpi }; since = -1
    job = { pkg, tol: tol ?? 0.001, steps: c.steps.map(s => ({ i: s.i, ev: s.ev, want: '/shots/' + s.shot })), results: [], running: true }
    runCase(act, c, { tol: job.tol, outDir: OUTWEB, onStep: r => job.results.push({ ...r, got: r.got && '/out/web/' + r.got }) })
      .catch(e => { job.error = e.message }).finally(() => { job.running = false })
    json(res, { ok: true, total: c.steps.length })
  },
  'GET /api/replay': async (req, res) => json(res, { ok: true, job }),
  // 保存此重录：草稿 → 正式 case。name 留空 = 覆盖原来的 <包名>.jsonl；截图目录一并替换。
  'POST /api/save': async (req, res) => {
    if (!rec || rec.n < 1) return json(res, { ok: false, err: '没有可保存的录制' })
    const { name: given } = await body(req)
    const name = (given || '').trim() || rec.pkg
    if (!validName(name)) return json(res, { ok: false, err: '用例名不能含 / \\、不能以 . 开头、不超过 80 字' })
    const existed = fs.existsSync(path.join(CASES, name + '.jsonl'))
    const dstDir = path.join(CASES, name)
    if (!dstDir.startsWith(CASES + path.sep)) return json(res, { ok: false, err: '路径越界' })
    fs.rmSync(dstDir, { recursive: true, force: true })
    let bytes = 0
    for (const l of rec.lines) if (l.type === 'step') {
      const n = path.basename(l.shot), data = fs.readFileSync(path.join(CASES, DRAFT, rec.pkg, n))
      save(`${name}/${n}`, data); bytes += data.length; l.shot = `${name}/${n}`
    }
    const file = save(name + '.jsonl', rec.lines.map(l => JSON.stringify(l)).join('\n') + '\n')
    for (const l of rec.lines) if (l.type === 'step') l.shot = `${rec.pkg}/${path.basename(l.shot)}`   // 草稿继续用自己的相对路径
    json(res, { ok: true, name, file, existed, steps: rec.n, bytes })
  },
  'POST /api/close': async (req, res) => { session = null; rec = null; json(res, await act.call({ cmd: 'close' })) },
  // “忽略行带”：同时写进 meta 与 open 步骤（回放的第 0 步也要用），之后每步的 wait 由网页带上
  'POST /api/ignore': async (req, res) => {
    const { ignore } = await body(req)
    if (!rec) return json(res, { ok: false, err: '没有正在录制的 App' })
    rec.lines[0].wait.ignore = ignore; rec.lines[1].wait.ignore = ignore
    json(res, { ok: true, jsonl: saveJsonl() })
  },
  // 手势：浏览器在松手时一次性给出整条轨迹，host 按时间轴注入（录制与回放走同一份 lib/steps.mjs，结果可复现）。
  // 按下时先取一棵节点树，用来给点按找选择器（此时画面还是操作前的）。
  'POST /api/g/down': async (req, res) => {
    const { x, y } = await body(req)
    g = { x, y, tree: side.call({ cmd: 'tree', full: false }).then(r => r.ok ? r.tree : null).catch(() => null) }
    json(res, { ok: true })
  },
  'POST /api/g': async (req, res) => {
    if (!rec) return json(res, { ok: false, err: '还没打开 App' })
    const { pts, wait } = await body(req)
    const step = classify(pts)
    if (step.ev === 'tap' && g) { const tree = await g.tree; step.sel = tree ? pickSel(tree, step.x, step.y) : null }
    g = null
    const r = await inject(act, step, { settle: wait || {}, stack: true, shot: true })
    if (!r.ok) return json(res, r)
    json(res, { ok: true, step: addStep({ ...step, res: r.settle, stack: r.stack, shot: r.shot }, wait) })
  },
  'POST /api/back': async (req, res) => {
    if (!rec) return json(res, { ok: false, err: '还没打开 App' })
    const { wait } = await body(req)
    const r = await inject(act, { ev: 'back' }, { settle: wait || {}, stack: true, shot: true })
    json(res, r.ok ? { ok: true, step: addStep({ ev: 'back', res: r.settle, stack: r.stack, shot: r.shot }, wait) } : r)
  },
}

http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x')
  try {
    if (req.method === 'GET' && url.pathname === '/') {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' }); return res.end(fs.readFileSync(path.join(here, 'recorder.html')))
    }
    if (req.method === 'GET' && url.pathname === '/api/stream') {
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' })
      subs.add(res); req.on('close', () => subs.delete(res)); pump(); return
    }
    if (req.method === 'GET' && url.pathname.startsWith('/shots/')) {
      const p = path.resolve(CASES, decodeURIComponent(url.pathname.slice(7)))
      if (!p.startsWith(CASES + path.sep) || !fs.existsSync(p)) return json(res, { ok: false }, 404)
      res.writeHead(200, { 'content-type': 'image/png' }); return res.end(fs.readFileSync(p))
    }
    if (req.method === 'GET' && url.pathname.startsWith('/out/web/')) {
      const p = path.resolve(OUTWEB, decodeURIComponent(url.pathname.slice(9)))
      if (!p.startsWith(OUTWEB + path.sep) || !fs.existsSync(p)) return json(res, { ok: false }, 404)
      res.writeHead(200, { 'content-type': 'image/png', 'cache-control': 'no-store' }); return res.end(fs.readFileSync(p))
    }
    const h = routes[`${req.method} ${url.pathname}`]
    if (!h) return json(res, { ok: false, err: 'not found' }, 404)
    await h(req, res, url)
  } catch (e) { console.error(e); if (!res.headersSent) json(res, { ok: false, err: String(e.message || e) }, 500) }
}).listen(PORT, '127.0.0.1', () => { console.log(`录制入口: http://localhost:${PORT}/\n保存目录: ${CASES}（已验证可写）\nhost 调试口: ${HOST_PORT}`); ensureHost().then(r => console.log('host 就绪：' + r.dev), e => console.log('⚠ ' + e.message)) })
