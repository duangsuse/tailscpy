#!/usr/bin/env node
// 由 doc/probe-<设备>.json 生成 doc/probe-<设备>.md（探针汇总：结果、通过/不通过、采用的回退）。
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
const doc = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'doc')
for (const f of fs.readdirSync(doc).filter(x => /^probe-.*\.json$/.test(x))) {
  const j = JSON.parse(fs.readFileSync(path.join(doc, f), 'utf8')), dev = f.slice(6, -5)
  const L = [`# 探针结果 · ${dev}`, '', '由 `node e2e/probe.mjs` 产出（原始数据 `' + f + '`），`node e2e/probe-md.mjs` 渲染。不通过的项后续任务以“采用的回退”为准。', '']
  const row = (a, b, c, d) => `| ${a} | ${b} | ${c} | ${d} |`
  L.push(row('探针', '结果', '通过 / 不通过', '采用的回退'), row('---', '---', '---', '---'))
  if (j.p1) { const n = j.p1.filter(x => x.verdict?.startsWith('精确')).length; L.push(row('P1 滚轮与远跳', `${j.p1.length} 个 App，${n} 个精确（明细见下）`, n === j.p1.length ? '通过' : '部分通过', '有动画 / 不支持 / 不精确的容器用分步拖动；不支持动作远跳的走连续大步滚轮')) }
  if (j.p2) L.push(row('P2 虚拟屏 A11y', `取树 IPC p50 ${j.p2.ipcMs.p50.toFixed(1)} ms / p95 ${j.p2.ipcMs.p95.toFixed(1)} ms（首次 ${j.p2.ipcMs.max.toFixed(0)} ms）；取树期间出 ${j.p2.framesDuringDump_p50} 帧；rowIndex ${j.p2.collectionItemInfo_rowIndex ? '有' : '无'}；${j.p2.nodes} 节点`, j.p2.verdict, j.p2.fallback))
  L.push(row('P3 超高虚拟屏 / configChanges', '未做：tall 默认关（GOAL §7 第 4 条），M4 任务 43 前不需要；需要 VirtualDisplay.resize 与采集 Surface 重建', '未判定', 'tall 关'))
  if (j.p4) L.push(row('P4 输入法策略', `ASCII 走 ${j.p4.ascii}，中文走 ${j.p4.chinese}；输入框内容 ${JSON.stringify(j.p4.editTextContent)}；虚拟屏 IME 策略设为 HIDE，之后 mInputShown=false`, j.p4.verdict, j.p4.fallback))
  L.push(row('P5 传输 / DERP 抖动', 'M1 任务 11 补', '待做', '—'))
  L.push(row('P6 栈回调', '未测回调；M0 全程用 `dumpsys` 查询（几十 ms，按需调用）', '未判定', '用查询'))
  if (j.p7) L.push(row('P7 熄屏', (j.p7.note || '') + ' ' + (j.p7.power || ''), j.p7.verdict, j.p7.fallback))
  if (j.p8) L.push(row('P8 设置还原（含看门狗）', `kill -9 后 ${j.p8.restoredAfterMs} ms 内四项设置回到原值（含 adb 往返）；原值 ${JSON.stringify(j.p8.orig)}`, j.p8.verdict, '—'))
  if (j.jpeg) L.push(row('JPEG 编码速度（B）', Object.entries(j.jpeg.results).map(([k, v]) => `${k}: ${v.msPerMP} ms/MP（${v.bytes} B）`).join('；') + '。' + j.jpeg.note, '已量', '用于定 `ahead_margin_ms` 与分片参数'))
  if (j.p1) {
    L.push('', '## P1 明细', '', '| App | 容器 | F_eff（像素/格） | 公式 (N+0.5)/F_eff 的 1/5/37/120/333 像素 | 结论 | 到边 | 远跳 |', '| --- | --- | --- | --- | --- | --- | --- |')
    for (const x of j.p1) L.push(`| ${x.pkg} | ${x.container || '—'} | ${x.calibration?.F_eff ?? '—'} | ${x.trials ? x.trials.map(t => t.dy).join(' / ') : '—'} | ${x.verdict || x.error} | ${x.edge ? x.edge.bounce : '—'} | ${x.scrollToPosition ? `${x.scrollToPosition.msg}；可见 rows ${x.scrollToPosition.visibleRows}（目标 ${x.scrollToPosition.target}）` : '—'} |`)
  }
  fs.writeFileSync(path.join(doc, f.replace(/\.json$/, '.md')), L.join('\n') + '\n'); console.log('写', f.replace(/\.json$/, '.md'))
}
