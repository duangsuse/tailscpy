#!/usr/bin/env zsh
# 在 B 上启动 host 并做 adb forward。用法: ./run.sh [stop] [端口=7701] [--record <B上目录>]
# 序列号取环境变量 B；只有一台设备时可省略。会先停掉同端口的旧 host，再推最新的 out/hj.dex（先停再推，60 §8）。
cd ${0:A:h}
STOP=; [ "$1" = stop ] && { STOP=1; shift; }
PORT=${1:-7701}; shift 2>/dev/null
S=${B:-$(adb devices | awk 'NR>1 && $2=="device"{print $1; exit}')}
[ -n "$S" ] || { echo "没有设备" >&2; exit 1; }
D=${HJDIR:-/data/local/tmp/hj}
A() { adb -s $S "$@"; }
# 只按 pid 文件杀自己那一个（不用 pkill -f，它会匹配到自己的命令行）
A shell "[ -f $D/pid.$PORT ] && kill \$(cat $D/pid.$PORT) 2>/dev/null; rm -f $D/pid.$PORT" >/dev/null
A forward --remove tcp:$PORT 2>/dev/null   # 只删自己这条
[ -n "$STOP" ] && exit 0
sleep 0.7
[ -s out/hj.dex ] || { echo "先跑 ./build.sh" >&2; exit 1; }
A shell "mkdir -p $D" && A push out/hj.dex $D/ >/dev/null
SO=; [ -s ../host-net/out/arm64-v8a/libhj_host.so ] && A push ../host-net/out/arm64-v8a/libhj_host.so $D/ >/dev/null && SO="--so $D/libhj_host.so"
A shell -n "cd $D && (setsid nohup app_process -Djava.class.path=$D/hj.dex /system/bin hj.MainKt --port $PORT $SO $* > $D/log.$PORT.txt 2>&1 < /dev/null & echo \$! > $D/pid.$PORT)"
A forward tcp:$PORT tcp:$PORT >/dev/null
for i in {1..40}; do sleep 0.25; PORT=$PORT node ../e2e/hc.mjs ping 2>/dev/null | grep -q "\"ok\":true" && { echo "host 就绪 localhost:$PORT（设备 $S）"; sleep 0.3; A shell "cat $D/url.txt 2>/dev/null" | tr -d '\r' | sed 's/^/  查看页 /'; exit 0; }; done
echo "host 没起来，日志:" >&2; A shell "cat $D/log.$PORT.txt" >&2; exit 1
