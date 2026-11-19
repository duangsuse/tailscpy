// host 调试口客户端（60 §10）：一行 JSON 请求 → 一行 JSON 响应。每个 Host 实例一条连接，请求串行。
import net from 'node:net'

export class Host {
  constructor(port = 7701) { this.port = port; this.q = Promise.resolve(); this.sock = null; this.buf = ''; this.wait = null }
  connect() {
    return new Promise((res, rej) => {
      const s = net.connect(this.port, '127.0.0.1')
      this.buf = ''
      s.setEncoding('utf8'); s.setNoDelay(true)
      s.on('connect', () => { this.sock = s; res(this) })
      s.on('error', e => { rej(new Error(`连不上 host（127.0.0.1:${this.port}）：${e.code || e.message}。先运行 host-adb/run.sh`)); this.wait?.rej(e) })
      s.on('close', () => { this.sock = null; this.wait?.rej(new Error('host 断开')) })
      s.on('data', d => {
        this.buf += d
        let i
        while ((i = this.buf.indexOf('\n')) >= 0) {
          const line = this.buf.slice(0, i); this.buf = this.buf.slice(i + 1)
          const w = this.wait; this.wait = null
          if (w) w.res(JSON.parse(line))
        }
      })
    })
  }
  /** 发一条命令，返回响应对象（ok:false 不抛，由调用方看）。 */
  call(req) {
    const p = this.q.then(async () => {
      if (!this.sock) await this.connect()   // host 重启过 / 连接断了：下次调用时自动重连
      return new Promise((res, rej) => {
      this.wait = { res, rej }
      this.sock.write(JSON.stringify(req) + '\n')
    }) })
    this.q = p.catch(() => {})
    return p
  }
  /** 同 call，但 ok:false 时抛错。 */
  async ok(req) { const r = await this.call(req); if (!r.ok) throw new Error(`${req.cmd}: ${r.err}`); return r }
  close() { this.sock?.destroy() }
}

export const sleep = ms => new Promise(r => setTimeout(r, ms))
