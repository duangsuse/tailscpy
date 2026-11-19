#!/usr/bin/env zsh
# 一键构建全部产物：rd.wasm + viewer、libhj_host.so（arm64）、hj.dex。
# 产物：viewer/dist/{index.html,main.js,rd.wasm}  host-net/out/arm64-v8a/libhj_host.so  host-adb/out/hj.dex
set -e
cd ${0:A:h}
echo "== rd → wasm";   cargo build --release -p rd --target wasm32-unknown-unknown 2>&1 | tail -1
mkdir -p viewer/dist && cp target/wasm32-unknown-unknown/release/rd.wasm viewer/dist/ && cp viewer/index.html viewer/dist/
bun build viewer/main.ts --outfile viewer/dist/main.js >/dev/null
echo "== host-net → .so (aarch64)"; cargo ndk -t arm64-v8a -o host-net/out build --release -p hj_host 2>&1 | tail -1
echo "== host-adb → dex"; host-adb/build.sh | tail -1
echo "== bench";       cargo build --release -p bench 2>&1 | tail -1
