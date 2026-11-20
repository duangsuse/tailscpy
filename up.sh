#!/usr/bin/env zsh
# 一条命令：（按需构建）→ 起 B 上的 host → 起静态服务器 → 自动显示 / 打开查看页 URL。
#   ./up.sh [--build] [--no-open] [--port 8765] [--app <包名>] [-- <传给 host 的参数>]
#   ./up.sh stop        停 host 和静态服务器
# 序列号取环境变量 B（只有一台设备可省略）。静态服务器优先用 `http-server -p 8765 -c 1`（没装就用 node viewer/serve.mjs）。
cd ${0:A:h}
PORT=8765; OPEN=1; BUILD=; APP=; HOSTARGS=(); RUN=.run; mkdir -p $RUN
case $1 in stop)
  host-adb/run.sh stop 2>/dev/null
  [ -f $RUN/static.pid ] && kill $(cat $RUN/static.pid) 2>/dev/null; rm -f $RUN/static.pid
  echo "已停止"; exit 0;; esac
while (( $# )); do case $1 in
  --build) BUILD=1;; --no-open) OPEN=;; --port) PORT=$2; shift;; --app) APP=$2; shift;;
  --) shift; HOSTARGS=($@); break;; *) echo "未知参数 $1" >&2; exit 2;; esac; shift; done

# 1. 构建（缺产物或要求时）
if [ -n "$BUILD" ] || [ ! -s host-adb/out/hj.dex ] || [ ! -s host-net/out/arm64-v8a/libhj_host.so ] || [ ! -s viewer/dist/main.js ]; then
  echo "== 构建"; ./build.sh || exit 1
fi

# 2. 静态服务器（已在跑就复用）
VIEWER=http://localhost:$PORT/
if ! curl -s -o /dev/null --max-time 1 $VIEWER; then
  if command -v http-server >/dev/null; then
    (cd viewer/dist && setsid nohup http-server -p $PORT -c 1 -s >/dev/null 2>&1 & echo $! > ../../$RUN/static.pid)
  else
    (setsid nohup node viewer/serve.mjs $PORT >/dev/null 2>&1 & echo $! > $RUN/static.pid)
  fi
  for i in {1..20}; do curl -s -o /dev/null --max-time 1 $VIEWER && break; sleep 0.25; done
fi
curl -s -o /dev/null --max-time 1 $VIEWER || { echo "静态服务器没起来（端口 $PORT 被占用？）" >&2; exit 1; }

# 3. host（带 --lan：开发时从本机浏览器经局域网连 B；默认只绑回环 + tailnet）
host-adb/run.sh 7701 --lan --viewer $VIEWER $HOSTARGS || exit 1
S=${B:-$(adb devices | awk 'NR>1 && $2=="device"{print $1; exit}')}
for i in {1..20}; do U=$(adb -s $S shell "cat /data/local/tmp/hj/url.txt 2>/dev/null" | tr -d '\r'); [ -n "$U" ] && break; sleep 0.25; done
[ -n "$U" ] || { echo "没读到 URL（看 B 上的 /data/local/tmp/hj/log.7701.txt）" >&2; exit 1; }
# 优先 tailnet，其次局域网
BEST=$(echo $U | awk '$1=="tailnet"{print $2; exit}'); [ -n "$BEST" ] || BEST=$(echo $U | awk '$1=="lan"{print $2; exit}')
[ -n "$BEST" ] || { echo "B 上没有可从本机访问的地址（需要 Wi-Fi 局域网或 tailnet）：" >&2; echo $U >&2; exit 1; }
[ -n "$APP" ] && BEST="$BEST&app=$APP"
echo; echo "查看页：$BEST"; echo
[ -n "$OPEN" ] && (command -v xdg-open >/dev/null && xdg-open "$BEST" >/dev/null 2>&1 &)
exit 0
