#!/usr/bin/env -S uv run --quiet --with websockets
"""活镜中继：B 机 scrcpy-server <-> 浏览器 WebSocket。

用法: relay.py [-s 序列号] [--size 1080x2400/420] [--app 包名] [--port 8000]
        [--server 路径] [--fake]   (--fake: 连 fake_server.py 在 27183 的假 server，不碰 adb)
协议（scrcpy 4.1，已在 Xiaomi/Android 16 上抓包核对）:
  video 套接字: 00 | 64B 设备名 | 4B codec | 12B 包头+载荷 ...
  包头: u64 flags/pts + u32 长度；flags bit63=session(宽u32,高u32 在包头后 8 字节)，bit62=config，bit61=关键帧
  control 套接字: 浏览器发来的字节原样写入；START_APP=16, RESET_VIDEO=17
浏览器收到的每条 WS 消息就是「12 字节包头+载荷」原样。
"""
import argparse, asyncio, http, os, signal, struct, subprocess, sys
from websockets.asyncio.server import serve

HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("-s", "--serial")
ap.add_argument("--size", default="1080x2400/420")
ap.add_argument("--app", default="com.android.settings")
ap.add_argument("--port", type=int, default=8000)
ap.add_argument("--server", default="/usr/share/scrcpy/scrcpy-server")
ap.add_argument("--fake", action="store_true")
args = ap.parse_args()

SCID = "%08x" % (os.getpid() & 0x7FFFFFFF)
ADB = ["adb"] + (["-s", args.serial] if args.serial else [])
clients = set()
ready = None  # asyncio.Event：收到第一个视频包后才算 server 就绪，过早的 RESET_VIDEO 会让它空指针崩溃
ctl_w = None
server_proc = None
fwd_ports = []  # 只清理自己建的 adb forward：--remove-all 会把别的程序（守护进程 8000/8001…）的转发也删掉


def adb(*a):
    return subprocess.run(ADB + list(a), check=True, capture_output=True, text=True).stdout.strip()


async def start_server():
    """push、forward、拉起 server（new_display 自己建一块虚拟屏），返回本机转发端口。"""
    if args.fake:
        return 27183
    adb("push", args.server, "/data/local/tmp/scrcpy-server.jar")
    port = int(adb("forward", "tcp:0", f"localabstract:scrcpy_{SCID}"))
    fwd_ports.append(port)
    global server_proc
    cmd = (f"CLASSPATH=/data/local/tmp/scrcpy-server.jar app_process / com.genymobile.scrcpy.Server 4.1 "
           f"scid={SCID} log_level=info tunnel_forward=true audio=false control=true new_display={args.size}")
    server_proc = await asyncio.create_subprocess_exec(
        *ADB, "shell", cmd, stdin=asyncio.subprocess.PIPE, stdout=sys.stderr, stderr=sys.stderr)  # stdin 不能是 EOF，否则远端 shell 退出
    return port


async def connect(port):
    for _ in range(75):  # server 没起来时 forward 会立刻断，整组重试
        ws = []
        try:
            vr, vw = await asyncio.open_connection("127.0.0.1", port); ws.append(vw)
            cr, cw = await asyncio.open_connection("127.0.0.1", port); ws.append(cw)
            await asyncio.wait_for(vr.readexactly(1), 5)  # dummy byte，读到才算 server 真的接了
            return vr, vw, cr, cw  # vw 必须留着引用：StreamWriter 被回收时会关掉连接
        except (OSError, asyncio.IncompleteReadError, asyncio.TimeoutError):
            for w in ws:
                w.close()
            await asyncio.sleep(0.3)
    raise SystemExit("连不上 scrcpy-server")


async def pump(vr):
    name = await vr.readexactly(64)
    codec = await vr.readexactly(4)
    print("device", name.rstrip(b"\0").decode(), codec.decode(), file=sys.stderr)
    while True:
        h = await vr.readexactly(12)
        flags, n = struct.unpack(">QI", h)
        # session 包（bit63）后面没有载荷，8 字节的宽高就在包头里；其余包 n 是载荷长度
        body = b"" if flags >> 63 else await vr.readexactly(n)
        msg = h + body
        ready.set()
        for ws in list(clients):
            try:
                await ws.send(msg)
            except Exception:
                clients.discard(ws)


async def drain(cr):
    while await cr.read(4096):
        pass


async def handler(ws):
    await ready.wait()
    clients.add(ws)
    ctl_w.write(bytes([17]))  # RESET_VIDEO：让新客户端拿到 session+config+关键帧
    try:
        async for m in ws:
            if isinstance(m, bytes):
                ctl_w.write(m)
    finally:
        clients.discard(ws)


def http_page(conn, req):
    if req.path == "/" or req.path == "/index.html":
        body = open(os.path.join(HERE, "index.html"), "rb").read()
        return _resp(body)
    if req.headers.get("Upgrade", "").lower() == "websocket":
        return None
    return conn.respond(http.HTTPStatus.NOT_FOUND, "not found\n")


def _resp(body):
    from websockets.http11 import Response
    from websockets.datastructures import Headers
    return Response(200, "OK", Headers([("Content-Type", "text/html; charset=utf-8"),
                                       ("Content-Length", str(len(body)))]), body)


async def main():
    global ctl_w, ready
    ready = asyncio.Event()
    port = await start_server()
    vr, vw, cr, ctl_w = await connect(port)
    tasks = [asyncio.create_task(pump(vr)), asyncio.create_task(drain(cr))]
    await ready.wait()
    pkg = args.app.encode()
    ctl_w.write(bytes([16, len(pkg)]) + pkg)  # START_APP：必须等 server 就绪，早了会被丢
    async with serve(handler, "127.0.0.1", args.port, process_request=http_page, max_size=None):
        print(f"http://localhost:{args.port}", file=sys.stderr)
        done, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        for t in done:
            print("任务结束:", t.get_coro().__name__, t.exception(), file=sys.stderr)
            t.result()


def cleanup():
    if server_proc and server_proc.returncode is None:
        server_proc.terminate()
    if not args.fake:
        for p in fwd_ports:
            subprocess.run(ADB + ["forward", "--remove", f"tcp:{p}"], capture_output=True)
        subprocess.run(ADB + ["shell", "pkill -f com.genymobile.scrcpy"], capture_output=True)


try:
    asyncio.run(main())
except KeyboardInterrupt:
    pass
except BaseException as e:
    import traceback; traceback.print_exc()
finally:
    cleanup()
