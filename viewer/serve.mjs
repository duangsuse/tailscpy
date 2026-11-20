#!/usr/bin/env node
// 开发用静态服务器：node viewer/serve.mjs [端口=8080]；提供 viewer/dist（http://localhost 是安全上下文，WebTransport 可用）
import http from 'node:http'; import fs from 'node:fs'; import path from 'node:path'; import { fileURLToPath } from 'node:url'
const dir = path.join(path.dirname(fileURLToPath(import.meta.url)), 'dist'); const port = +(process.argv[2] || 8080)
const T = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.wasm': 'application/wasm' }
http.createServer((q, r) => {
  const f = path.join(dir, q.url.split(/[?#]/)[0] === '/' ? 'index.html' : q.url.split(/[?#]/)[0])
  if (!f.startsWith(dir) || !fs.existsSync(f)) { r.writeHead(404).end('404'); return }
  r.writeHead(200, { 'content-type': T[path.extname(f)] || 'application/octet-stream', 'cache-control': 'no-store' }); fs.createReadStream(f).pipe(r)
}).listen(port, '127.0.0.1', () => console.log(`查看页 http://localhost:${port}/`))
