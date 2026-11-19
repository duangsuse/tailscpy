# 活镜投屏

把 Android 设备（B）上**单个白名单 App** 投到浏览器（A）：A 里点 / 滑 / 返回，B 的画面以单画布回传。目标是在 300–800 KB/s 的 Tailscale 链路上做到文字逐像素锐利、见过的内容来回滚不再下行流量、返回上一页立即出现。

- 规则与总览：[GOAL.md](GOAL.md)（金标准是 [IDEA.md](IDEA.md)），展开文档在 [doc/](doc/)，任务顺序见 [doc/90-tasks.md](doc/90-tasks.md)（含 M0 进度表）。
- 现在做到 **M0（工具链）**：host（虚拟屏、采集、A11y、注入、设置还原）、人工录制器与回放、语料录制、探针、bench 骨架。传输和 viewer 是 M1 的事。

## 打开录制器

```sh
export B=<adb 序列号>        # 只有一台设备可省略
node e2e/recorder.mjs        # 浏览器打开 http://localhost:8800/
```

host 没起会自动运行 `host-adb/run.sh`；保存目录默认 `e2e/cases`（`--cases <目录>` 可改），启动时会验证可写。

1. 下拉框选 App，即打开并开始录制。
2. 左键按下到松手是一步操作；**右键或 Esc = 返回**。每步先等“画面变化超过阈值 → 再静止”，然后截图。
3. 点 **保存此重录**：`prompt()` 里填用例名，**留空 = 覆盖原来的 `<包名>.jsonl`**。
4. 选已录的 case 点 **回放**，逐步看 ✓/✗、差异比例、录制/回放截图；鼠标悬停步骤列表，右侧大图预览。

产出：`e2e/cases/<用例名>.jsonl` + `<用例名>/NNNN.png`（截图已 gitignore，只有 jsonl 入库）。格式与已知限制见 [e2e/README.md](e2e/README.md)。命令行回放：`node e2e/run.mjs [--repeat 3]`。

## 查看录制结果的统计数据

人工录制只给“回放是否一致”（上面的网页回放 / `node e2e/run.mjs`）。**字节、耗时、峰值这类统计来自语料录制 + bench**：

```sh
# 1. 用场景 runner 录语料（B 端 host 录帧 / 树 / 输入，拉回本机）
node e2e/scenario.mjs enter-back com.android.settings --rec --corpus ../corpus
# 2. 看时间线是否合理
node e2e/replay.mjs list ../corpus/trace/<设备>/com.android.settings/enter-back/<批次>
# 3. 出报表（朴素基线编码 + 网络模型 + x264 对照）
cargo run --release -p bench -- ../corpus/trace/<设备>/com.android.settings/enter-back/<批次> --out bench/out
```

报表在 `bench/out/<日期>-<git 短 hash>/`：

| 文件 | 内容 |
|---|---|
| `report.md` / `report.json` | 每分钟用法加权字节（KB/min）、首屏字节 vs 90 KB 预算线、按步骤 / 按 Op 分的字节、“输入→画面稳定”耗时分布、码率与队列峰值、逐步正确性、x264 对照、与上一份报表的差值。`report.json` 无墙钟，同一输入两次逐字节相同 |
| `frames.csv` | 每帧 seq、时间、字节、到达 A 的时间 |
| `timing.json` | 编码 / 解码 / x264 耗时（墙钟，不确定） |

示例（设置 App 的 `enter-back`，400 KB/s）：324 帧、下行 3182.6 KB、4206 KB/min、逐步正确性 14/14、x264 1478.7 KB。网络参数 `--bw 400KB/s --rtt 40ms --jitter 10ms --seed 1`；任意一帧导出 PNG：`node e2e/replay.mjs export <trace目录> <帧号> out.png`。

<details>
<summary>工具链安装</summary>

```sh
mise use -g rust@stable
rustup target add aarch64-linux-android wasm32-unknown-unknown
mise use -g "cargo:cargo-ndk@4"
mise use -g android-sdk@23.0        # 然后装：platforms;android-35  build-tools;35.0.0  ndk;30.0.16248370
export ANDROID_HOME=$(mise where android-sdk) ANDROID_NDK_HOME=$ANDROID_HOME/ndk/30.0.16248370
mise use -g kotlin@2.4.21           # JDK 用系统自带（java 17+）
mise use -g node@latest bun@latest  # node 跑 e2e，bun 打包 viewer/main.ts
```

另需 adb；ffmpeg 给 bench 的 x264 对照用（没有就跳过）。
</details>

<details>
<summary>构建并推到 B</summary>

```sh
./build.sh                       # 一键：rd.wasm + viewer、libhj_host.so（aarch64）、hj.dex、bench
host-adb/run.sh [端口=7701]      # 先停同端口旧 host → 推 hj.dex + libhj_host.so → 起 host → adb forward
host-adb/run.sh stop [端口]      # 停（只按 pid 文件杀自己那一个）
host-adb/build.sh                # 只构建 dex
cargo ndk -t arm64-v8a -o host-net/out build --release -p hj_host
cargo build --release -p rd --target wasm32-unknown-unknown
```
</details>

<details>
<summary>调试口（host 的 127.0.0.1:7701，一行 JSON）</summary>

```sh
node e2e/hc.mjs ping
node e2e/hc.mjs apps                                 # 本机已装的白名单 App
node e2e/hc.mjs open pkg=com.android.settings        # 改设置 → 建虚拟屏 → 连 A11y → 冷启动 App
node e2e/hc.mjs png out.png                          # 当前帧（无状态栏，1080x2640 无损 PNG）
node e2e/hc.mjs tree                                 # 节点树摘要（含 rows= / rowIndex / scroll=F|B）
node e2e/hc.mjs stack                                # 栈深与栈顶 Activity
node e2e/hc.mjs '{"cmd":"tap","x":540,"y":1066,"settle":{},"stack":true,"shot":true}'
                                                     # 点按 → 等“变化阈值 + 静止” → 栈 → 截图（settle 参数见 doc/60-host.md 实现备注）
node e2e/hc.mjs '{"cmd":"wheel","x":500,"y":1600,"v":-0.6}'   # 滚轮，v 单位是“格”，精确像素公式见 e2e/lib/wheel.mjs
node e2e/hc.mjs back | key code=4 | text s=你好 | clipget | viewcfg | close
node e2e/hc.mjs rec_start dir=/data/local/tmp/hj/rec/x  ...  rec_stop   # 语料录制（80 §3）；也可 host 启动参数 --record <目录>
```
</details>

<details>
<summary>通用场景与语料</summary>

```sh
node e2e/scenario.mjs cold-open                      # 每个白名单 App 跑一遍；也可指定：scroll-down-up com.android.settings
node e2e/scenario.mjs all com.android.settings --rec --twice    # 同时录语料到 ../corpus/trace/<设备>/<包>/<场景>/<批次>/；连跑两次比对
node e2e/replay.mjs list <trace目录>                 # 事件时间线摘要 + 时间戳单调检查
node e2e/replay.mjs export <trace目录> <帧号> out.png # 任意一帧导出 PNG（与当时画面一致）
node e2e/replay.mjs pull <B上目录> <本机目录>
```
</details>

<details>
<summary>探针与测试</summary>

```sh
node e2e/probe.mjs all|p1|p2|p4|p7|p8|jpeg [pkg...]  # 结果写 doc/probe-<设备>.json，汇总见 doc/probe-<设备>.md
node e2e/probe-md.mjs                                # 由 JSON 重新生成 doc/probe-<设备>.md
cargo run --release -p bench -- --sample --out /tmp/bench-out    # 合成 trace，免真机
cargo test -p bench -p rd
```
</details>

<details>
<summary>目录</summary>

| 目录 | 内容 |
|---|---|
| `host-adb/` | Kotlin（app_process）：虚拟屏、采集、A11y、注入、设置还原、录制、调试口 |
| `rd/` | Rust：wire / 编解码 / 页面状态机（两端和 bench 共用；M1 起填） |
| `host-net/` | Rust：和 rd 链进同一个 .so（M0 只有 JNI 空壳） |
| `viewer/` | TS + rd.wasm（M0 只有空壳） |
| `bench/` | Rust：帧重放、朴素基线、网络模型、报表、x264 对照 |
| `e2e/` | JS：调试口客户端、人工录制器、回放、场景 runner、探针 |
| `cmp-scp/`、`old/`、`old-ov/` | 参考 / 对比用，不要改 |
</details>
