#!/usr/bin/env zsh
# host-adb：Kotlin → dex（无构建系统，kotlinc + d8）。
# 用法: ./build.sh [序列号]   给了序列号就把 hj.dex 推到 B 的 $HJDIR（默认 /data/local/tmp/hj）。
# 往运行中的实例目录推新文件会让它崩（进程正映射着旧文件，60 §8）：host 在运行时用 ./run.sh 的“先停再推”。
set -e
cd ${0:A:h}
SDK=${ANDROID_HOME:-$HOME/.local/share/mise/installs/android-sdk/23.0}
AJAR=$SDK/platforms/android-35/android.jar
D8=$SDK/build-tools/35.0.0/d8
KLIB=$(dirname $(dirname $(command -v kotlinc)))/lib
mkdir -p out
kotlinc -jvm-target 11 -cp $AJAR -d out/classes.jar src/*.kt 2>&1 | grep -v -E "^warning:|^\s|^$" || true
[ -s out/classes.jar ]
# d8 读不懂 Kotlin 2.x 的元数据只是警告，产物不受影响
$D8 --release --min-api 29 --lib $AJAR --output out out/classes.jar $KLIB/kotlin-stdlib.jar 2>&1 | grep -v -E "^\s+at |kotlin.Metadata|parsing kotlin metadata|^Info: |Kotlin metadata|Should never be called|^Warning: Unexpected error|^$" || true
[ -s out/classes.dex ]
mv out/classes.dex out/hj.dex
ls -la out/hj.dex
if [ -n "$1" ]; then
  HD=${HJDIR:-/data/local/tmp/hj}
  adb -s $1 shell "mkdir -p $HD" && adb -s $1 push out/hj.dex $HD/ >/dev/null && echo "已推送到 $1:$HD/hj.dex"
fi
