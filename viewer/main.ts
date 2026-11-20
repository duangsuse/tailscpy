// 活镜查看端（M1）：WebTransport + rd.wasm 解码 + 画布 + 输入 + l / r 调试面。
// 线程：M1 全在主线程（Worker / OPFS 是 M3）。wasm 里存画布，JS 只按脏矩形 putImageData。
const $ = (id: string) => document.getElementById(id)!
const VER = 1

// ───────── 启动：URL → 连接参数 ─────────
const params = new URLSearchParams(location.hash.slice(1))
const B = params.get('b'), HASH = params.get('h'), TOKEN = params.get('t')
const hexBytes = (h: string) => Uint8Array.from(h.match(/../g)!.map(x => parseInt(x, 16)))
function setMsg(t: string) { const m = $('msg'); m.textContent = t; m.style.display = t ? 'flex' : 'none' }
function toast(t: string, ms = 3000) { const e = $('toast'); e.textContent = t; e.style.display = 'block'; clearTimeout((e as any)._t); (e as any)._t = setTimeout(() => e.style.display = 'none', ms) }
function status(t: string, bad = false) { const e = $('st'); e.textContent = t; e.className = bad ? 'bad' : '' }

// ───────── rd.wasm ─────────
const wasm = (await WebAssembly.instantiate(await (await fetch('./rd.wasm')).arrayBuffer(), {})).instance.exports as any
const mem = () => new Uint8Array(wasm.memory.buffer)
const enc = new TextEncoder(), dec = new TextDecoder()
function withBytes<T>(b: Uint8Array, f: (p: number, n: number) => T): T { const p = wasm.rd_alloc(b.length); mem().set(b, p); try { return f(p, b.length) } finally { wasm.rd_free(p, b.length) } }
const outText = (n: number) => dec.decode(mem().slice(wasm.rd_out_ptr(), wasm.rd_out_ptr() + n))
function upBytes(msg: any): Uint8Array {
  const n = withBytes(enc.encode(JSON.stringify(msg)), (p, l) => wasm.rd_up_encode(p, l))
  if (n < 0) throw new Error('Up 编码: ' + outText(wasm.rd_out_len ? wasm.rd_out_len() : 0) + ' ' + JSON.stringify(msg))
  return mem().slice(wasm.rd_out_ptr(), wasm.rd_out_ptr() + n)
}

// ───────── 状态 ─────────
const S: any = {
  w: 0, h: 0, connected: false, editable: false, apps: [] as any[], params: [] as any[], pkg: '', stats: null as any,
  rxBytes: 0, rxWin: [] as [number, number][], txMsgs: 0, verifyFail: 0, applyErr: '', lastSeq: 0, rtt: [] as number[], probe: '', mask: 0, replaced: false,
  sc: null as any, vy: 0, follow: true, vel: 0, missing: 0, lastUser: 0, lastScrollT: 0, viewSent: 0, imgFail: 0,
}
;(window as any).__hj = { S, sendUp: (m: any) => send(m), canvasHash: () => canvasHash(), scrollBy: (dy: number) => userScroll(dy), viewHash: () => viewHash() }
let cv = $('cv') as HTMLCanvasElement, ctx = cv.getContext('2d')!
const ov = $('ov') as HTMLCanvasElement, octx = ov.getContext('2d')!

// ───────── 连接 ─────────
let wt: any = null, ctlW: WritableStreamDefaultWriter, upW: WritableStreamDefaultWriter
let backoff = 500
const send = (m: any) => {
  if (!S.connected) return
  const name = typeof m === 'string' ? m : Object.keys(m)[0]
  const w = ['Hello', 'Inventory', 'Open', 'Close', 'Tune', 'Ping', 'Probe'].includes(name) ? ctlW : upW
  w.write(upBytes(m)).catch(() => {})
}

async function connect() {
  if (!B || !HASH || !TOKEN) { setMsg('缺少连接参数。\n用 B 上打印的 URL 打开：#b=<地址:端口>&h=<证书hash>&t=<token>'); return }
  status('连接中…'); setMsg('连接中…')
  try {
    wt = new (window as any).WebTransport(`https://${B}/`, { serverCertificateHashes: [{ algorithm: 'sha-256', value: hexBytes(HASH) }] })
    await wt.ready
  } catch (e: any) { return failed('连不上 B：' + (e?.message || e)) }
  const bi = await wt.createBidirectionalStream(); ctlW = bi.writable.getWriter()
  upW = (await wt.createUnidirectionalStream()).getWriter()
  S.connected = true
  ctlW.write(upBytes({ Hello: { ver: VER, token: TOKEN, cache_id: cacheId(), cache_rev: 0 } }))
  readStream(bi.readable, onDown)
  ;(async () => { const r = wt.incomingUnidirectionalStreams.getReader(); let first = true
    while (true) { const { value, done } = await r.read(); if (done) break
      if (first) { first = false; readStream(value, onTx) } else readProbe(value) } })()
  wt.closed.then(() => failed('连接已断开'), (e: any) => failed('连接已断开：' + (e?.message || e)))
  setInterval(() => { if (S.connected) { send({ Ping: { t: Math.round(performance.now() * 1000) } }) } }, 2000)
}
function failed(m: string) {
  if (!S.connected && !wt) return
  S.connected = false; wt = null; status(m, true)
  if (S.replaced) { setMsg('已在别处打开'); return }
  setMsg(m + '\n重连中…'); setTimeout(connect, backoff); backoff = Math.min(backoff * 2, 8000)
}
function cacheId() { let c = ''; try { c = localStorage.getItem('hj_cache_id') || '' } catch {} if (!c) { c = crypto.randomUUID(); try { localStorage.setItem('hj_cache_id', c) } catch {} } return c }

async function readStream(rs: ReadableStream<Uint8Array>, on: (body: Uint8Array) => void) {
  const r = rs.getReader(); let buf = new Uint8Array(0)
  try {
    while (true) {
      const { value, done } = await r.read(); if (done) break
      S.rxBytes += value.length; S.rxWin.push([performance.now(), value.length])
      const nb = new Uint8Array(buf.length + value.length); nb.set(buf); nb.set(value, buf.length); buf = nb
      while (buf.length >= 4) {
        const n = new DataView(buf.buffer, buf.byteOffset).getUint32(0, true)
        if (buf.length < 4 + n) break
        on(buf.slice(4, 4 + n)); buf = buf.slice(4 + n)
      }
    }
  } catch {}
}
let probeStart = 0
async function readProbe(rs: ReadableStream<Uint8Array>) {
  const r = rs.getReader(); let n = 0; const t0 = performance.now()
  while (true) { const { value, done } = await r.read(); if (done) break; n += value.length }
  const dt = (performance.now() - t0) / 1000
  S.probe = `下行 ${(n / 1024).toFixed(0)} KB / ${dt.toFixed(2)}s = ${(n / 1024 / dt).toFixed(0)} KB/s`; renderPanel()
}

// ───────── 下行 ─────────
let queue: Promise<void> = Promise.resolve()
const early: Uint8Array[] = []   // Opened（ctl）可能晚于第一批 tx 到达：先攒着
function onTx(body: Uint8Array) { if (!S.w) { early.push(body); return } queue = queue.then(() => applyTx(body)) }

function onDown(body: Uint8Array) {
  const n = withBytes(body, (p, l) => wasm.rd_down_apply(p, l))
  if (n < 0) { S.applyErr = outText(0); return }
  const m = JSON.parse(outText(n)); const name = typeof m === 'string' ? m : Object.keys(m)[0]; const v = typeof m === 'string' ? null : m[name]
  switch (name) {
    case 'Welcome': S.apps = v.apps; S.params = v.params; setSb(); fillApps(); status('已连接'); backoff = 500; setMsg('选择右上角的 App'); autoOpen(); break
    case 'Opened': openCanvas(v.geom.w, v.geom.h); S.pkg = v.pkg; ($('app') as HTMLSelectElement).value = v.pkg; setMsg(''); status(v.pkg); drainEarly(); break
    case 'Focus': S.editable = v.editable; $('foc').textContent = v.editable ? '⌨ 输入中' : ''; break
    case 'Clip': navigator.clipboard.writeText(v.s).catch(() => toast('B 的剪贴板：' + v.s.slice(0, 40) + '（点此复制）', 6000)); break
    case 'Stats': S.stats = v; renderPanel(); break
    case 'Params': S.params = v.params; setSb(); renderPanel(); break
    case 'Fault': toast(faultText(v.code, v.msg), 5000); if (v.code === 'AppGone') { setMsg('会话已结束，请重新选择 App'); S.pkg = ''; ($('app') as HTMLSelectElement).value = '' } break
    case 'Replaced': S.replaced = true; setMsg('已在别处打开'); break
    case 'Pong': { const rtt = (performance.now() * 1000 - v.t) / 1000; S.rtt.push(rtt); if (S.rtt.length > 200) S.rtt.shift(); break }
  }
}
function faultText(code: string, msg: string) {
  return ({ OffSession: '该操作超出会话允许的应用范围，已返回', AppGone: '应用已退出', BlankFrame: 'B 端画面为空（受保护内容或 App 未启动）', Stale: '内容已过期，请再点一次',
    BadToken: 'token 不对', BadVersion: '版本不一致', Busy: 'B 忙' } as any)[code] + (msg ? '：' + msg : '')
}

function setSb() { const q = S.params.find((x: any) => x.key === 'sb_w'); if (q && S.w) wasm.rd_set_sb(q.val) }
function openCanvas(w: number, h: number) {
  S.w = w; S.h = h; wasm.rd_canvas_init(w, h); S.sc = null; S.vy = 0; S.follow = true; setSb()
  cv.width = ov.width = w; cv.height = ov.height = h
  const wrap = $('wrap'); const fit = () => { const st = $('stage'); const s = Math.min(st.clientWidth / w, st.clientHeight / h); wrap.style.width = w * s + 'px'; wrap.style.height = h * s + 'px'; S.scale = s; renderPanel() }
  fit(); (window as any).onresize = fit
  ctx.clearRect(0, 0, w, h); octx.clearRect(0, 0, w, h)
}
function drainEarly() { while (early.length) { const b = early.shift()!; queue = queue.then(() => applyTx(b)) } }

let dirty: [number, number, number, number] | null = null
const flashes: any[] = []
async function applyTx(body: Uint8Array) {
  const n = withBytes(body, (p, l) => wasm.rd_down_apply(p, l))
  if (n < 0) { S.applyErr = outText(0); send({ Resync: { page: 1 } }); return }
  const m = JSON.parse(outText(n)); const t = m.Tx
  if (!t) { refreshScroll(); scheduleRender(); return }   // Page / Regions 等
  S.txMsgs++; S.lastSeq = t.seq
  for (const r of t.rects) { if (r.r === 0) addDirty(r.x, r.y, r.w, r.h); flashes.push({ ...r, t: performance.now() }) }
  if (t.verify_fail.length) { S.verifyFail += t.verify_fail.length; console.warn('校验失败', t.verify_fail) }
  // 有损图片：wasm 不带 JPEG 解码，浏览器原生解（createImageBitmap）后写回画布
  for (const im of t.images) {
    const p = wasm.rd_image_ptr(im.i), l = wasm.rd_image_len(im.i)
    const jpg = mem().slice(p, p + l)
    try {
      const bmp = await createImageBitmap(new Blob([jpg], { type: 'image/jpeg' }))
      const oc = new OffscreenCanvas(im.w, im.h); const c2 = oc.getContext('2d')!; c2.drawImage(bmp, 0, 0, im.w, im.h)
      const px = c2.getImageData(0, 0, im.w, im.h).data
      withBytes(new Uint8Array(px.buffer), (pp) => wasm.rd_put_rgba(im.r, im.x, im.y, im.w, im.h, pp))
    } catch (e) { S.imgFail++; console.warn('图片解码失败', e) }
  }
  send({ Ack: { seq: t.seq } })
  refreshScroll(); scheduleRender()
}
function addDirty(x: number, y: number, w: number, h: number) {
  if (!dirty) dirty = [x, y, x + w, y + h]; else { dirty[0] = Math.min(dirty[0], x); dirty[1] = Math.min(dirty[1], y); dirty[2] = Math.max(dirty[2], x + w); dirty[3] = Math.max(dirty[3], y + h) }
}
let rafPending = false
function scheduleRender() { if (!rafPending) { rafPending = true; requestAnimationFrame(render) } }
const img = () => new ImageData(new Uint8ClampedArray(wasm.memory.buffer, wasm.rd_view_ptr(), S.w * S.h * 4), S.w, S.h)
function render() {
  rafPending = false
  if (!S.w) return
  // 画布 + 视口处的滚动行 + Overlay 合成一帧（没有 Scroll 区域时视口无意义）
  S.missing = wasm.rd_compose(S.sc ? Math.round(S.vy) : 0); ctx.putImageData(img(), 0, 0); dirty = null
  drawOverlay()
}
function canvasHash(): string {   // 测试用：整个画布的快速摘要
  if (!S.w) return ''
  const b = new Uint8Array(wasm.memory.buffer, wasm.rd_canvas_ptr(), S.w * S.h * 4); let h = 2166136261 >>> 0
  for (let i = 0; i < b.length; i += 97) h = Math.imul(h ^ b[i], 16777619) >>> 0
  return h.toString(16)
}
function viewHash(): string {   // 测试用：当前显示的画面（含本地滚动后的合成）
  if (!S.w) return ''
  wasm.rd_compose(S.sc ? Math.round(S.vy) : 0)
  const b = new Uint8Array(wasm.memory.buffer, wasm.rd_view_ptr(), S.w * S.h * 4); let h = 2166136261 >>> 0
  for (let i = 0; i < b.length; i += 97) h = Math.imul(h ^ b[i], 16777619) >>> 0
  return h.toString(16)
}

// ───────── Scroll 区域：本地滚动（30 §2）─────────
function refreshScroll() {
  const n = wasm.rd_scroll_info()
  if (n < 0) { S.sc = null; return }
  const o = JSON.parse(outText(n)); const old = S.sc
  const sameCols = old && old.rect[0] === o.rect[0] && old.rect[2] === o.rect[2]
  if (sameCols && old.rect[1] !== o.rect[1]) S.vy += o.rect[1] - old.rect[1]   // 吸顶 / 吸底的行被切出去：区域上沿下移，视口（区域顶行的画布 y）跟着加
  const first = !old || !sameCols
  S.sc = { rect: o.rect, head: o.head, ymin: o.y_min, ymax: o.y_max, atTop: o.at_top, atBot: o.at_bottom }
  if (first) { S.vy = o.head; S.follow = true; sendView() }   // 跟随时也报视口：B 才知道目标就是现在的采集头（否则它会去追旧目标）
  else if (S.follow) { if (S.vy !== o.head) { S.vy = o.head; sendView() } }
  else if (Math.abs(S.vy - o.head) < 1 && performance.now() - S.lastUser > 300) S.follow = true   // B 追上了，且用户停手一会儿：恢复跟随
}
function clampVy(v: number) {
  const s = S.sc; if (!s) return v
  // 到顶 / 到底就停在已知边缘；否则最多比已知范围多滚一屏（B 的采集头要追，滚太远只会一片占位）
  const rh = s.rect[3]
  const lo = s.atTop ? s.ymin : s.ymin - rh, hi = s.atBot ? s.ymax - rh : s.ymax
  return Math.min(Math.max(v, lo), Math.max(hi, lo))
}
function sendView() {   // 视口变了就上报（只保留最新值，≤ 每 30 ms 一次）
  if (!S.sc) return
  const now = performance.now(); if (now - S.viewSent < 30) { setTimeout(sendView, 30 - (now - S.viewSent)); return }
  S.viewSent = now; send({ View: { page: 1, region: 1, y: Math.round(S.vy), vy: S.vel } })
}
function userScroll(dy: number) {   // dy：设备像素，正 = 内容上移（往下滚）
  if (!S.sc) return
  S.follow = false; S.lastUser = S.lastScrollT = performance.now()
  S.vy = clampVy(S.vy + dy); sendView(); scheduleRender()
}
let fling = 0
function startFling(v: number) {   // v：像素 / ms
  cancelAnimationFrame(fling); if (Math.abs(v) < 0.05) return
  let last = performance.now()
  const step = (t: number) => {
    const dt = Math.min(50, t - last); last = t; const before = S.vy   // 标签页在后台时 rAF 停摆，回来第一帧 dt 很大
    S.vy = clampVy(S.vy + v * dt); v *= Math.pow(0.995, dt)   // 摩擦
    S.lastUser = S.lastScrollT = t; sendView(); scheduleRender()
    if (Math.abs(v) < 0.03 || S.vy === before) { S.vel = 0; return }
    S.vel = v; fling = requestAnimationFrame(step)
  }
  fling = requestAnimationFrame(step)
}
const inScrollY = (p: { x: number, y: number }) => !!S.sc && p.y >= S.sc.rect[1] && p.y < S.sc.rect[1] + S.sc.rect[3]
const inScroll = (p: { x: number, y: number }) => inScrollY(p) && p.x < S.w - 56
const onThumbTrack = (p: { x: number, y: number }) => inScrollY(p) && p.x >= S.w - 56   // 右缘一条：拖自绘滚动条（本地跳转，30 §2）
function thumbTo(py: number) {   // 指针 y（设备像素）→ 视口：在已知范围内按比例跳
  const s = S.sc; if (!s) return; const [, ry, , rh] = s.rect; const range = Math.max(s.ymax - s.ymin, rh); if (range <= rh) return
  const th = Math.max(48, rh * rh / range); const f = Math.min(1, Math.max(0, (py - ry - th / 2) / (rh - th)))
  S.follow = false; S.lastUser = S.lastScrollT = performance.now(); S.vy = clampVy(s.ymin + f * (range - rh)); sendView(); scheduleRender()
}

// ───────── r 遮罩 + 自绘滚动条 ─────────
const KC: any = { Fill: '#4cf', 'Lit.Palette': '#4f8', 'Lit.Residual': '#fd4', 'Lit.Raw': '#f84', Image: '#f4c', Copy: '#fa4', Overlay: '#8f4' }
function toScreenY(r: any) { return r.r === 1 && S.sc ? r.y - Math.round(S.vy) + S.sc.rect[1] : r.y }
function drawOverlay() {
  octx.clearRect(0, 0, S.w, S.h)
  if (S.mask === 2) drawLayers()
  if (S.mask === 1) {
    const now = performance.now()
    for (let i = flashes.length - 1; i >= 0; i--) { const f = flashes[i], a = 1 - (now - f.t) / 1500; if (a <= 0) { flashes.splice(i, 1); continue }
      const y = toScreenY(f); if (y + f.h < 0 || y > S.h) continue
      octx.globalAlpha = a; octx.strokeStyle = KC[f.k] || '#fff'; octx.lineWidth = 4; octx.strokeRect(f.x + 2, y + 2, f.w - 4, f.h - 4) }
    octx.globalAlpha = 1
    if (flashes.length) scheduleRender()
  }
  // 自绘滚动条：按已知范围显示位置，停手 1 s 后淡出
  const s = S.sc
  if (s) { const age = performance.now() - S.lastScrollT, a = Math.max(0, Math.min(1, 1.5 - age / 700))
    if (a > 0) {
      const [, ry, , rh] = s.rect; const range = Math.max(s.ymax - s.ymin, rh); const th = Math.max(48, rh * rh / range)
      const pos = range > rh ? Math.min(1, Math.max(0, (S.vy - s.ymin) / (range - rh))) : 0
      octx.globalAlpha = a * 0.7; octx.fillStyle = '#ddd'; octx.fillRect(S.w - 14, ry + pos * (rh - th), 8, th); octx.globalAlpha = 1
      if (a < 1) scheduleRender()
    } }
}
function drawLayers() {   // 图 / 文 / 视三类区：按每个像素最后一次写入的层着色（bg 绿、lit 紫）；滚动区按当前视口合成
  wasm.rd_compose(S.sc ? Math.round(S.vy) : 0)
  const l = new Uint8Array(wasm.memory.buffer, wasm.rd_view_layer_ptr(), S.w * S.h); const im = octx.createImageData(S.w, S.h); const d = new Uint32Array(im.data.buffer)
  for (let i = 0; i < l.length; i++) d[i] = l[i] === 1 ? 0x3c40ff40 : l[i] === 2 ? 0x50ff40ff : 0
  octx.putImageData(im, 0, 0)
}
function setMask(m: number) { S.mask = m; scheduleRender(); toast(['遮罩：关', '遮罩：最近刷新的矩形（蓝=Fill 绿=调色板 黄=残差 紫=图片 橙=搬移）', '遮罩：图/文分层（绿=无损 紫=有损图片）'][m]) }

// ───────── App 选择 ─────────
function fillApps() {
  const s = $('app') as HTMLSelectElement; s.length = 1
  for (const a of S.apps) { const o = document.createElement('option'); o.value = a.pkg; o.textContent = a.name === a.pkg ? a.pkg : `${a.name}（${a.pkg}）`; s.appendChild(o) }
  s.value = S.pkg
}
($('app') as HTMLSelectElement).onchange = (e) => { const p = (e.target as HTMLSelectElement).value; if (!p) { send('Close'); return } location.hash = location.hash.replace(/&?app=[^&]*/, '') + '&app=' + p; openApp(p) }
function openApp(p: string) { setMsg('打开 ' + p + ' …'); S.w = 0; send({ Open: { pkg: p, geom: null } }) }
function autoOpen() { const a = new URLSearchParams(location.hash.slice(1)).get('app'); if (a && S.apps.some((x: any) => x.pkg === a)) openApp(a) }

// ───────── 输入 ─────────
const wrap = $('wrap')
const pos = (e: MouseEvent) => { const r = wrap.getBoundingClientRect(); return { region: 0, x: Math.round((e.clientX - r.left) * S.w / r.width), y: Math.round((e.clientY - r.top) * S.h / r.height) } }
let down: any = null
const LONG_MS = 500, SLOP = 8
wrap.addEventListener('contextmenu', e => e.preventDefault())
// 内容坐标：Scroll 区域里的点 y 是画布 y（视口 + 屏幕行），其余是屏幕坐标
const atOf = (p: { x: number, y: number }) => inScroll(p) ? { region: 1, x: p.x, y: Math.round(Math.round(S.vy) + (p.y - S.sc.rect[1])) } : { region: 0, x: p.x, y: p.y }
wrap.addEventListener('pointerdown', e => {
  $('ime').focus()
  if (!S.w || !S.connected) return
  if (e.button === 2) { send('Back'); return }
  if (e.button !== 0) return
  cancelAnimationFrame(fling); S.vel = 0
  wrap.setPointerCapture(e.pointerId)
  const p = pos(e), local = inScroll(p)
  if (onThumbTrack(p)) { down = { thumb: true, dot: dot(e) }; thumbTo(p.y); return }
  down = { x: e.clientX, y: e.clientY, p, at: atOf(p), drag: false, scroll: false, long: false, local, dot: dot(e), samples: [[performance.now(), e.clientY]] }
  down.timer = setTimeout(() => { if (down && !down.drag && !down.scroll) { down.long = true; send({ LongPress: { page: 1, at: down.at } }) } }, LONG_MS)
})
wrap.addEventListener('pointermove', e => {
  if (!down) return
  if (down.thumb) { thumbTo(pos(e).y); return }
  const dx = e.clientX - down.x, dyc = e.clientY - down.y
  if (!down.drag && !down.scroll && Math.hypot(dx, dyc) > SLOP && !down.long) {
    clearTimeout(down.timer)
    if (down.local && Math.abs(dyc) >= Math.abs(dx)) { down.scroll = true; down.lastY = down.y }   // 竖直为主 → 本地滚动（带惯性）
    else { down.drag = true; send({ Touch: { page: 1, phase: 0, at: down.p, dt: 0 } }); down.t = performance.now() }
  }
  if (down.scroll) {
    const sc = 1 / (S.scale || 1); userScroll(-(e.clientY - down.lastY) * sc); down.lastY = e.clientY
    down.samples.push([performance.now(), e.clientY]); if (down.samples.length > 6) down.samples.shift()
  } else if (down.drag) { const now = performance.now(); send({ Touch: { page: 1, phase: 1, at: pos(e), dt: Math.round(now - down.t) } }); down.t = now }
})
wrap.addEventListener('pointerup', e => {
  if (!down) return
  if (down.thumb) { setTimeout(() => down?.dot?.remove(), 0); down = null; return }
  clearTimeout(down.timer)
  if (down.scroll) {
    const s = down.samples, a = s[0], b = s[s.length - 1]; const dt = b[0] - a[0]
    if (dt > 0 && performance.now() - b[0] < 80) startFling(-((b[1] - a[1]) / dt) / (S.scale || 1))
  } else if (down.drag) send({ Touch: { page: 1, phase: 2, at: pos(e), dt: Math.round(performance.now() - down.t) } })
  else if (!down.long) send({ Tap: { page: 1, at: down.at } })
  setTimeout(() => down?.dot?.remove(), 0); down = null
})
function dot(e: PointerEvent) { const d = document.createElement('div'); const r = wrap.getBoundingClientRect()
  d.style.cssText = `position:absolute;width:20px;height:20px;border-radius:50%;background:#fff6;left:${e.clientX - r.left - 10}px;top:${e.clientY - r.top - 10}px;pointer-events:none`
  wrap.appendChild(d); setTimeout(() => d.remove(), 600); return d }   // 按下反馈：短暂的圆点
wrap.addEventListener('wheel', e => {
  e.preventDefault(); if (!S.w) return
  const p = pos(e)
  if (inScroll(p) && e.deltaX === 0) { cancelAnimationFrame(fling); userScroll(e.deltaY / (S.scale || 1)); return }   // 滚动区里的滚轮：本地滚动，B 的采集头去追
  send({ Wheel: { page: 1, at: atOf(p), dx: e.deltaX / 100, dy: e.deltaY / 100 } })
}, { passive: false })

const KEYS: any = { Enter: 66, Backspace: 67, Tab: 61, ArrowUp: 19, ArrowDown: 20, ArrowLeft: 21, ArrowRight: 22, Delete: 112, Home: 122, End: 123, PageUp: 92, PageDown: 93, Space: 62 }
const ime = $('ime') as HTMLTextAreaElement
function togglePanel() { const p = $('panel'); p.style.display = p.style.display === 'block' ? 'none' : 'block'; renderPanel() }
$('bl').onclick = togglePanel; $('br').onclick = () => setMask((S.mask + 1) % 3)
$('toast').onclick = () => $('toast').style.display = 'none'
window.addEventListener('keydown', e => {
  if (e.altKey && e.key.toLowerCase() === 'l') { e.preventDefault(); return togglePanel() }
  if (e.altKey && e.key.toLowerCase() === 'r') { e.preventDefault(); return setMask((S.mask + 1) % 3) }
  if (!S.editable && !e.ctrlKey && !e.altKey && !e.metaKey) { if (e.key === 'l') return togglePanel(); if (e.key === 'r') return setMask((S.mask + 1) % 3) }
  if (e.key === 'Escape') { e.preventDefault(); return void send('Back') }
  if (!S.connected || !S.w) return
  if (e.ctrlKey && !e.altKey && /^[a-z]$/i.test(e.key)) {
    const k = e.key.toLowerCase(); if (k === 'v') return   // 在 paste 事件里处理
    if ('cxa'.includes(k)) { e.preventDefault(); send({ Key: { code: k.charCodeAt(0) - 97 + 29, down: true, meta: 0x1000 } }); send({ Key: { code: k.charCodeAt(0) - 97 + 29, down: false, meta: 0x1000 } }) }
    return
  }
  const code = KEYS[e.code] ?? KEYS[e.key]
  if (code !== undefined && S.editable) { e.preventDefault(); send({ Key: { code, down: true, meta: 0 } }); send({ Key: { code, down: false, meta: 0 } }) }
})
ime.addEventListener('input', () => { const t = ime.value; ime.value = ''; if (t && S.editable) send({ Text: { s: t } }) })
ime.addEventListener('paste', e => { e.preventDefault(); const t = e.clipboardData?.getData('text') || ''
  if (t) { send({ ClipSet: { s: t } }); send({ Key: { code: 50, down: true, meta: 0x1000 } }); send({ Key: { code: 50, down: false, meta: 0x1000 } }) } })
document.addEventListener('visibilitychange', () => { if (!document.hidden) scheduleRender() })

// ───────── l 面板 ─────────
const fmtB = (n: number) => n < 1024 ? n + ' B' : n < 1048576 ? (n / 1024).toFixed(1) + ' KB' : (n / 1048576).toFixed(2) + ' MB'
const pct = (a: number[], p: number) => { const s = [...a].sort((x, y) => x - y); return s.length ? s[Math.min(s.length - 1, Math.floor(s.length * p))] : 0 }
function renderPanel() {
  const p = $('panel'); if (p.style.display !== 'block') return
  const st = S.stats; const L: any = {}; if (st) for (const [k, v] of st.layers) L[k] = v
  const X: any = {}; if (st) for (const [k, v] of st.extra) X[k] = v
  const tot = (L.bg || 0) + (L.lit || 0) + (L.fg || 0) || 1
  const now = performance.now(); S.rxWin = S.rxWin.filter((x: any) => now - x[0] < 5000); const rate = S.rxWin.reduce((a: number, x: any) => a + x[1], 0) / 5
  const bar = (name: string, c: string) => `<div class="row"><span>${name}</span><span>${fmtB(L[name] || 0)}（${(100 * (L[name] || 0) / tot).toFixed(0)}%）</span></div><div class="bar"><i style="width:${100 * (L[name] || 0) / tot}%;background:${c}"></i></div>`
  const ctl = S.params.map((q: any) => `<div class="row"><span title="${q.key}">${q.label}</span><span><input type="range" data-k="${q.key}" min="${q.min}" max="${q.max}" step="${q.step}" value="${q.val}"> <b>${q.val}</b></span></div>`).join('')
  p.innerHTML = `<h4>三层（会话累计）</h4>${bar('bg', '#4c8')}${bar('lit', '#c4c')}${bar('fg', '#48c')}
    <div class="row"><span>省流分项</span><span>M2 起（滚动 / 页缓存 / 行引用）</span></div>
    <h4>链路</h4>
    <div class="row"><span>下行码率(5s)</span><span>${fmtB(rate)}/s</span></div>
    <div class="row"><span>累计下行 / 帧数</span><span>${fmtB(S.rxBytes)} / ${X.frames ?? 0}</span></div>
    <div class="row"><span>编码耗时(最近/累计)</span><span>${(X.enc_ms ?? 0).toFixed(1)} / ${(X.enc_ms_total ?? 0).toFixed(0)} ms</span></div>
    <div class="row"><span>未确认字节</span><span>${fmtB(X.inflight ?? 0)}</span></div>
    <div class="row"><span>RTT 中位/P95</span><span>${pct(S.rtt, .5).toFixed(0)} / ${pct(S.rtt, .95).toFixed(0)} ms</span></div>
    <div class="row"><span>校验失败 / 图片解码失败</span><span>${S.verifyFail} / ${S.imgFail}</span></div>
    ${S.sc ? `<h4>滚动区</h4><div class="row"><span>区域 y / 高</span><span>${S.sc.rect[1]} / ${S.sc.rect[3]}</span></div><div class="row"><span>采集头 / 视口</span><span>${S.sc.head - 1048576} / ${Math.round(S.vy) - 1048576}${S.follow ? '（跟随）' : ''}</span></div><div class="row"><span>已知范围</span><span>${S.sc.ymin - 1048576} … ${S.sc.ymax - 1048576}</span></div><div class="row"><span>到顶 / 到底 / 缺行</span><span>${S.sc.atTop ? '是' : '否'} / ${S.sc.atBot ? '是' : '否'} / ${S.missing}</span></div>` : ''}
    <div class="row"><span>显示缩放</span><span>${((S.scale || 0) * 100).toFixed(0)}%${S.scale && S.scale !== 1 ? '（非 1:1，文字会被重采样）' : ''}</span></div>
    <div><button id="pr">网络探针 P5</button> <span>${S.probe}</span></div>
    <h4>参数</h4>${ctl}<h4>操作</h4><button id="rc">断开重连</button>`
  p.querySelectorAll('input[type=range]').forEach((el: any) => el.onchange = () => { send({ Tune: { scope: 'Global', key: el.dataset.k, val: +el.value } }) })
  ;($('pr')).onclick = async () => { S.probe = '测量中…'; for (let i = 0; i < 40; i++) { send({ Ping: { t: Math.round(performance.now() * 1000) } }); await new Promise(r => setTimeout(r, 60)) }
    send({ Probe: { bytes: 4 << 20 } }) }
  ;($('rc')).onclick = () => { try { wt?.close() } catch {} }
}
setInterval(renderPanel, 1000)

connect()
