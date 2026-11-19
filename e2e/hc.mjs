#!/usr/bin/env node
// 调试口命令行：node e2e/hc.mjs '{"cmd":"ping"}'   或   node e2e/hc.mjs ping   或   node e2e/hc.mjs open pkg=com.android.settings
// 取帧存文件：node e2e/hc.mjs png out.png   ；节点树摘要：node e2e/hc.mjs tree
import fs from 'node:fs'
import { Host } from './lib/host.mjs'

const port = +(process.env.PORT || 7701)
const [a, ...rest] = process.argv.slice(2)
const h = await new Host(port).connect()
let req
if (a?.startsWith('{')) req = JSON.parse(a)
else if (a === 'png') {
  const r = await h.ok({ cmd: 'frame', fmt: 'png' })
  fs.writeFileSync(rest[0] || 'frame.png', Buffer.from(r.data, 'base64'))
  console.log(`seq=${r.seq} ${r.w}x${r.h} → ${rest[0] || 'frame.png'}`); h.close(); process.exit(0)
} else if (a === 'tree') { const r = await h.ok({ cmd: 'tree', brief: true }); console.log(r.text, `\nipc ${r.ipcMs.toFixed(1)} ms @seq ${r.seq}`); h.close(); process.exit(0) }
else {
  req = { cmd: a }
  for (const kv of rest) { const [k, ...v] = kv.split('='); const s = v.join('='); req[k] = s !== '' && !isNaN(+s) ? +s : s === 'true' ? true : s === 'false' ? false : s }
}
const r = await h.call(req)
if (r.data && r.data.length > 200) r.data = `<${r.data.length} base64>`
if (r.shot) r.shot = `<${r.shot.length} base64>`
console.log(JSON.stringify(r)); h.close()
