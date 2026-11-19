# 活镜 控制面协议

类型定义就是协议：`proto/src/lib.rs`（crate `hj-proto`）。线格式在 `proto/src/wire.rs`。本文只讲取舍、通道和规则。数据面（布局层/精灵/视频的帧内容）另文，由 `relay/bench/codec.py` 的 10 个操作码移植，不在这里。

旧的按行 eval 协议（`[s1] obj.method(JSON)`、`=N`/`=EN`，Java 实现）已删，历史在 git `a6fa99d`。它为「B 端手写解析器、JS 求值、Java 客户端」设计，换成 Rust + WebTransport 后三者都不存在了。

## 位置

```
A: 页面(Rust→WASM) ──WebTransport(QUIC)──► B: Rust(.so，app_process 内) ──JNI──► Kotlin 平台层
                                                       解码枚举、调度          VirtualDisplay / 注入 / UiAutomation
```

- QUIC 终点在 B 的 Rust 里（wtransport）；页面直连 B，不经 A 端原生代理。
- 线上字节只有 Rust 解析。Kotlin 看到的是已解码的参数（`inject(sid, phase, id, x, y, pressure, tMs)` 这类 JNI 调用），不碰字节。
- 编码：serde + postcard。同一批类型再 derive 出 JSON，用于 `hjctl`、日志、pipefile，以及 `addJavascriptInterface`（桥只收字符串：`hj.send(jsonString)`）。**JSON 不是线协议**，只是同一枚举的文本形式。验收：同一段 JSON 经 `hjctl` 发给 B，和经 WebView 桥发出，效果一致。

## 通道

每条流的第一个字节是类型（`wire::stream`）。

| 通道 | 方向 | 可靠性 | 内容 | 频率 |
|---|---|---|---|---|
| `ctl` (0) | 双向，A 先开 | 可靠有序 | `ClientMsg` / `ServerMsg`：握手、请求/应答、事件 | 低 |
| `up` (1) | A→B 单向 | 可靠有序 | `Up`：输入、`Shown` 帧确认、`Miss` 精灵未命中、`Verify` 帧缓冲哈希（调试） | 高 |
| `frames` (2) | B→A 单向 | 可靠有序 | 布局层 + 精灵池的帧事务（层间原子提交）。消息 = `fid u32 | flags u8 | epoch u32 | origin i64 | 操作流`：`origin` = 内容坐标（屏幕第 0 行 = 内容第几行），`epoch` = 内容空间代号（换页时 B 递增，A 据此清地图）。两端据此维护同一张 tile 地图 | 高，数据面 |
| `video` (3) | B→A，每帧一条单向流 | 过期即 reset | 视频区域的硬编码帧，帧间互不阻塞 | 高，数据面 |
| 数据报 | 双向 | 不可靠 | `Up` 的子集：只含 Move 的 `InputFrame`、`Shown`。**v1 不启用**，要先量 | — |

分三条而不是一条的原因：`ctl` 里会有几十 KB 的应用列表和库存列表，不能挡住每 8ms 一次的触摸采样。

（v2 曾有的大对象通道 `blob` 与 APNG 导出已在 v3 删除：导出改成 A 端从自己的 tile 地图拼长截图。）

## 帧与版本

- 流上每条消息 = LEB128 长度 + postcard；数据报 = 裸 postcard。`wire::Reader` 增量解帧，吃任意切块。上限 `MAX_CTL` = 1 MiB；超限、长度前缀畸形、解码失败，流已失步，**关流**。
- postcard 不自描述，所以：枚举变体**只追加**，结构体字段不增删改序，不兼容就升 `VERSION`；禁用 serde 的 tag/untagged/flatten/skip_serializing_if/default 属性；热路径变体排在枚举最前。A、B 是同一个 APK，握手版本不等直接 `Reject`。
- `tests/roundtrip.rs` 的 `ups()/clients()/servers()` 必须覆盖每个变体（往返 postcard 和 JSON），`golden_bytes` 钉住最热消息的线上字节。加变体要补样例。
- `VERSION` 现在是 **3**：

| 版本 | 变更 |
|---|---|
| v2 | 删 A11y 树（`A11yTree/A11yNode/A11yDump/A11yWatch/Event::A11y/Res::A11y`）、剪贴板（`Clip/ClipData/Req::Clip/Event::Clip`）和 blob 通道（WS 标签 4） |
| **v3** | **合成层调试整体删除**：只留**只读**层列表（`Req::Debug(DebugCmd::List)` → `Res::LayerList`）；层开关、层归属、APNG 导出、`LayerDump/Region/Dumped`、`LayerInfo.on` 全删（mm1 的查看器不做合成层调试）。帧消息加**内容原点** |

  删变体是破坏性改动，只有在两端同一个 APK、一起升版本时才允许。

## 内容坐标与 tile 地图

每一帧都带**内容原点** `origin`：本帧屏幕第 0 行在内容空间里的行号（可负）。

- 滚动 = 原点变化，tile 不动 → 不用重发像素。
- tile id = 内容哈希 → 同一内容两端自动去重。
- 长截图 = 内容空间本身（A 端 `Decoder::long_shot()`，B 端 `Encoder::long_shot()`）。
- B 端的地图状态：`GET /tilemap` → `{origin, segments:[{lo,n,id}]}`。
- 换页（内容被整体替换）时 B 把 `epoch` 加一；A 端收到新 `epoch` 就清空自己的 tile 地图
  （否则两套坐标系的行混在一起，长截图会错位）。

细节见 `mm1/doc/25-tile-map.md`。

## 监听地址与两种传输

| 地址 | 传输 | 用途 |
|---|---|---|
| `127.0.0.1:<端口>` | WS(TCP) | 开发通道：`adb forward` / `adb reverse` |
| Tailscale 网卡 IP 的同一端口 | WS(TCP) | 产品路径（老）：`ws://<ip>:<端口>/?t=<令牌>` |
| Tailscale 网卡 IP 的同一端口 | **WebTransport(QUIC/UDP)** | 产品路径（新）：`https://<ip>:<端口>/?t=<令牌>`，浏览器用 `serverCertificateHashes` 固定自签证书 |

**不得绑 0.0.0.0**；设备上没有 `tailscale0` 时只留回环（会打印提示）。

### WebTransport（QUIC）

- **为什么**：TCP 有队头阻塞（手机上丢一个包，后面全等）；浏览器端的**证书固定**只有 WebTransport 提供。
- **ALPN** 必须是 `h3`；客户端先开一条双向流，上面是 HTTP/3 的 `CONNECT`（`:protocol = webtransport`），
  服务端回 `:status = 200` 之后这条流就是数据通道（`mm1/host/src/quic.rs` 手写了最小 HTTP/3 层，quinn 不带）。
- **消息格式**：一条消息 = `u32 小端长度 | 通道标签 | 载荷`（和 WASM 侧 out 缓冲同格式，WS 那边是每个消息一个二进制帧）。
- **证书**：启动时现生成一张自签证书（SAN = localhost + Tailscale IP + 127.0.0.1），
  指纹（DER 的 SHA-256，base64）打印在日志里，也从 `GET /cert` 取（`{"sha256":"…"}`）。
- **A 端**：页面优先试 WebTransport（`navigator.webTransport.connect` + `serverCertificateHashes`），
  失败自动退回 WebSocket（`?force=ws` 可强制）。
- **写是异步的**：同步路径（编码线程）只往队列里塞，写由专门的 tokio 任务负责；读到的消息交给同步线程按序处理。

> 实测：真 Chrome（`--enable-quic`）下握手、开会话、收帧、逐帧 0px 校验全通过；
> 设备（razr 40）没装 Tailscale，走不了这条路，WS 路径不受影响。

## 握手与鉴权

1. A 开 `ctl`，发 `Hello{version, token, name, caps}`。`caps` 是查看器能解的视频编码和字面量编码。
2. B 回 `Welcome`（含权限档 `Tier`、B 时钟 `host_ms`、文件服务地址）或 `Reject{Version|Token|Busy}`。
3. 之后 `Ping`/`Pong` 估 RTT 与时钟偏移（Touch 的 `t` 靠它换算）。

信任不靠 token 本身：配对时二维码交给 A 一份 `{host, port, B 证书 SHA-256, token}`，页面用 `serverCertificateHashes` 固定证书（自签、有效期 ≤14 天，轮换方式未定，见末尾），所以 token 只经过已认证的 TLS。B 的 UDP 端口**只绑 Tailscale 网卡**，不得 0.0.0.0（沿用旧约定）。

## 请求/应答 vs 无应答

- `ctl` 上 `Req{rid, req}` 必有 `Res{rid, Result<Res, Failure>}`。类型已决定有无返回，不需要旧协议的 fn/proc 声明表。
- `up` 上全部无应答（高频）。失败经 `Event::Fault{sid, code, msg}` 异步回报。

## 输入

- `Touch` 是单指采样（id + phase + 设备像素坐标 f32 + 压力 + A 时钟 `t`），多指由 B 按 id 聚合成 MotionEvent（同 scrcpy 的做法）。B 用 `t + 时钟偏移` 当 eventTime 注入，网络抖动不影响 fling 速度。**未实测**。
- `Gesture`（Tap/Swipe/Scroll/Back）由 B 合成，A 不必串流原始采样。`Scroll` 是手指按住、逐步移动、不 fling 的精确位移（`rig.py` 已验证这样位移确定）。
- `InputFrame.seen` = A 生成事件时屏幕上显示的帧号。转场合成、旧图 TTFP 期间屏幕与 B 不一致，B 可据 `seen` 丢弃落在已变化区域上的 Down，回 `Fault(Stale)`。**策略待实测**，字段先留。
- 虚拟屏没有 HOME；返回就是 `Key(KEYCODE_BACK)` 或 `Gesture::Back`。`Event::Foreground.at_root` 为真时 A 应结束会话而不是再发返回（根页面按返回会让 B 上的应用退出，已踩过）。

## 会话

- 一个会话 = 一块虚拟屏 + 一个应用，`sid` 由 B 分配，所有 `up`/`ctl` 消息带 `sid`。同时开几个会话未定。
- `OpenSession{pkg, user, display, fps_cap, resume}`：display 按查看器物理像素。`resume` 命中则 `Opened.resumed = true`，B 在 A 断开期间保留虚拟屏和应用（`SessionState::Detached`），暂停编码。
- `Resync`：`Frame` 重发整个布局层、`Video` 要视频关键帧、`Cache` A 丢了精灵池。`Inventory` 在连接/会话开始时上报持久缓存里的哈希；引用未命中走 `Up::Miss`。线上精灵 ID 是内容哈希（截断 64 位），所以 B 不再是 A 池的唯一写者。
- `Shown{fid, t, decode_us}` 是 A 每显示一帧回一条：B 用来背压（A 端大帧解码中位 76ms）和量端到端延迟。

## 热路径字节数（含长度前缀，`hot_path_sizes` 测试）

| 消息 | 字节 |
|---|---|
| 单个 Touch Move（一次 `InputFrame`） | 20 |
| 4 个 Touch Move 合批 | 65 |
| Tap 手势 | 15 |
| `Shown` | 9 |

QUIC/UDP/IP 头约 40 字节，所以合批比单发更划算；每个 JS tick 的 `getCoalescedEvents()` 合成一个 `InputFrame`。

## 跳转栈与返回缓存（`Event::Stack`）

- B 的 Kotlin（`mm1/daemon/src/Stack.kt`）在**输入抬手/按键之后**异步读 `dumpsys activity activities`，取虚拟屏那一段第一个有 `Hist #N` 的任务：深度 = Hist 条数，栈顶 = 最上面那条的 `包/类`。一次约 50–80ms，抬手后 150ms 与 600ms 各查一次；深度或栈顶变了才发 `Event::Stack{sid, depth, top}`。页面左下角显示「栈 N 类名」（`?` = 还没收到；构建名挪到调试面板的统计行）。
- **只反映 Activity 栈**：同一个 Activity 里的 Fragment / Compose 导航深度不变，那类跳转只靠画面判断，不影响正确性。
- 返回缓存不依赖栈，栈只是让它更准：整页被换掉（中间带里 ≥ 一半的行变了、不是滚动、上一页自上次整页级变化起已稳定 ≥ `Config::page_stable_ms`=120ms）时，B 发 `RETAIN` 把上一页整屏登记进精灵池；返回（或再次进入）时整屏 `SPRITE_REF` 命中。按压高亮、光标、时钟这类局部小变化不重置「稳定」计时。
- 池里的页面带深度标签：**标签 = 最近一次输入开始时的深度**（B 查到的新深度常常比换页第一帧还早到，用登记时的深度会把刚被换掉的页标成「更深」）。池满时淘汰顺序：比当前更深的（已返回掉的页）→ 与当前同深的（当前页自己的滚动条带）→ 更浅的页面最后动；同档内最久没用的先走。深度未知则纯 LRU。
- 整帧哈希（`Up::Verify`、`Encoded.hash`、条带 id）改为各行哈希的折叠（`hj_codec::frame_hash` / `Decoder::hash()`），B 不再单独读一遍画面。

## 滚动画布与多位移（编码器行为，操作码不变）

- **多位移**：一帧里可以有多个 COPY 且位移不同（折叠标题栏 + 列表、分屏）。`find_motion` 每轮取得票最多的位移，认下它能匹配的连续行段后，这些行退出下一轮投票；最多 `shift_rounds` 轮（默认 3）。COPY 本来就读上一帧，所以多个互不重叠的目标段不需要新操作。
- **滚动画布**：滚动帧里，B 把上一帧滚动区域中「内容坐标上还没有」的行用 `RETAIN id y h` 登记成一小段条带（条带 id = 这些行整行哈希的折叠，同内容自动去重）。内容坐标 `cy = 屏幕行 + o`，`o` 每个滚动帧按主位移更新；B 维护「内容坐标 → 条带」的跨度表。露出的行先按坐标查表（哈希相等才用，所以坐标错了只是不命中），命中发 `SPRITE_REF`，哪怕只有几行；剩下的再走老的行哈希投票。**A 端不知道画布的存在**，它只看到普通的 RETAIN/SPRITE_REF/DROP，所以协议没有变化。
- 原点丢了（过度滚动拉伸、跳变等「没认出滚动的整页变化」）：下一个滚动帧按内容投票重新找原点，找不到就当换了页、画布重来。FRAME_REF 回到某个历史帧时原点跟着回去（历史项带 `o`）。
- 调小池容量：B 立即按淘汰顺序腾空间，欠 A 的 `DROP` 放进下一帧操作流开头（FRAME_REF 帧不能带别的操作，所以顺延）。
- 长截图：`Encoder::long_shot()` 把页眉/页脚和画布里按内容坐标拼起来的滚动内容叠成一张高图（只在 B 端，不经 A）。

## 运行时调参（HTTP，调试用，不在协议枚举里）

**分应用的优化参数**：`POST /tune?app=<包名>&k=..&v=..` 只改这个 App 的参数；不带 `app` 是全局。
`GET /tune?app=<包名>` 返回"默认值 + 全局覆盖 + 该 App 覆盖"后的参数表；新会话开这个 App 时按这套值建编码器。
页面（客户端 JS 管道）可以按 App 调：换 App 时参数表跟着换。

- `GET /tune` → `{"params":[{key,label,group,val,min,max,step}…]}`。参数表由 B 的编码器发布（`Encoder::params()`），页面据此用 lil-gui 画控件，所以加参数不用改页面。
- `POST /tune?t=<令牌>&k=<键>&v=<数>` → 改一项（越界夹到范围内；未知键 400），返回最新参数表；令牌不对 403。改动由编码线程在下一轮循环应用，设置记在服务端，**新会话沿用**。`k=longshot`（不带 v）返回长截图 PNG（没滚动过 404）。
- 下表是 `Encoder::params()` 的全部可调项（`codec/tests/roundtrip.rs::protocol_md_lists_every_tunable` 逐行核对键、默认值、范围、步长，改了代码不改这里测试会失败）：

| 键 | 默认 | 范围 | 步长 | 组 | 含义 |
|---|---|---|---|---|---|
| `pool_mb` | 64 | 0–160 | 4 | 缓存 | 精灵池容量（MB，A、B 各存一份）；0 = 不缓存。调小立即淘汰，欠 A 的 DROP 随下一帧发出。上限 160 是因为 A 的硬上限是 256MB |
| `canvas` | 1 | 0–1 | 1 | 缓存 | 滚动画布开关；0 = 老办法（每次滚动开始登记一整屏，回程靠行哈希投票） |
| `canvas_min_rows` | 4 | 1–64 | 1 | 缓存 | 画布命中的最短连续行数 |
| `page_stable_ms` | 120 | 0–1000 | 10 | 缓存 | 整页换掉时，上一页稳定显示多久才登记进池（返回缓存）；0 = 关 |
| `retain_gap_ms` | 150 | 0–1000 | 10 | 缓存 | 仅老办法（`canvas`=0）：距上一个滚动帧多久算一次新的滚动 |
| `motion` | 1 | 0–1 | 1 | 滚动 | 滚动检测（COPY）总开关 |
| `shift_rounds` | 3 | 1–6 | 1 | 滚动 | 一帧最多找几个不同的位移；1 = 整屏只一个 |
| `min_votes` | 24 | 4–200 | 1 | 滚动 | 位移投票的最低票数 |
| `min_seg` | 16 | 2–128 | 1 | 滚动 | COPY 段的最短行数 |
| `edge_copy` | 1 | 0–1 | 1 | 滚动 | 左右边缘带（滚动条滑块一类，不跟内容走）里的竖直平移：整条用小矩形 COPY 搬，只补两端残差 |
| `zstd_level` | 3 | 1–19 | 1 | 编码 | 字面量 zstd 级别（级别 19 压得小但 p90 要 325ms） |

- 只在页面里生效、不经 B 的调节：**滚轮倍率**（`cli.wheelMul`，乘在滚轮事件的位移上，存 localStorage）。历史环长度 `hist` 不能运行时改（A 的解码器在开会话时定长）。
- 调试面板里的只读状态来自 `GET /stats`：`pool_bytes`（当前缓存）、`pool_limit`、`pool_strips`、`canvas_spans`（画布跨度数）、`canvas_extent`（画布覆盖的内容行数，含空洞）、`canvas_origin`（当前屏第 0 行的内容坐标）、`stack_depth`、`frames`、`bytes`。

### `GET /frames`（每帧明细，e2e 分析用）

返回 `{"cols":[…],"rows":[[…]…]}`，最近 4000 帧。列（`host/tests/server_e2e.rs::protocol_md_lists_every_frames_column` 核对本表）：

| 列 | 含义 |
|---|---|
| `fid` `t_us` `bytes` `enc_us` | 帧号、发出时刻（自服务启动 µs）、操作流字节、编码耗时 |
| `copies` `shift` `shifts` | COPY 条数、主位移（>0 内容向下）、本帧用了几个不同位移 |
| `lits` `lit_px` `fills` `ref` | 字面量矩形数及像素数、FILL 数、是否 FRAME_REF |
| `rows_dirty` `rows_resid` | 与上一帧不同的行数、平移/贴池之后残差还剩的行数 |
| `b_hist` `b_copy` `b_fill` `b_lit_ui` `b_lit_img` `b_pool` | 各层写出的字节（含 RETAIN/DROP/SPRITE_REF 的 `b_pool`） |
| `pool_rows` `canvas_rows` | 从池里贴回的行数、其中按画布坐标命中的行数 |
| `cv_none` `cv_diff` `cv_short` | 滚动区域里没贴上画布的脏行：该坐标没有 / 有但内容不同 / 命中段短于最短行数 |
| `edge_copy_rows` | 边缘带小矩形 COPY 搬了多少行 |
| `lit_edge_px` `lit_edge_bytes` | 完全落在左右 40px 边缘带（`hash::band` 之外，滚动条/卡片圆角所在）里的字面量矩形：像素数、字节数（已含在 `bytes` 里） |

## 没做 / 待定

- 数据报快路径：全部 `Up` 消息自包含（坐标是绝对值），可无改动地拆到数据报。要先量流的队头阻塞在 Tailscale 上是否真的可见。
- A11y：v2 已把协议里的 A11y 树/订阅删掉（运行时本来也没实现）。以后要做「按元素点」「文字/图片分类」再从零加回协议，届时注意 A11y 滞后，只能当提示，不能当真相。
- 证书轮换：自签证书 ≤14 天就要换，A 如何拿到新哈希（配对时多发几个、还是连接内下发）未定，目前枚举里没有对应消息。
- `Tier::Root` 排到后期（目前只有一台设备有 ROOT），到时在枚举末尾追加。
- wasm32：本机没装 `wasm32-unknown-unknown`，**未验证**能编到 wasm；`aarch64-linux-android` 已 `cargo check` 通过。
- 文件服务不在本协议内（miniserve 兼容的 HTTP），`Welcome.files` 只告诉 A 地址。

## 与旧协议的对应

| 旧 | 新 |
|---|---|
| `auth("令牌")` | `Hello` / `Welcome` / `Reject` |
| `ses.open` / `ses.close` / `ses.recent` | `Req::OpenSession` / `CloseSession` / `ListSessions` |
| `[s1]` 会话前缀 | 每条消息的 `sid` 字段 |
| `inp.pointers` / `inp.gesture` / `inp.key` / `inp.text` / `inp.scroll` | `Up::Input` 里的 `Touch` / `Gesture` / `Key` / `Text` / `Wheel` |
| `=N json` / `=EN json` | `ServerMsg::Res{rid, Ok/Err}` |
| 裸 `json` 事件 | `ServerMsg::Ev` |
| 声明表（哪些调用要等） | 不需要：`up` 全无应答，`ctl` 全有应答 |
