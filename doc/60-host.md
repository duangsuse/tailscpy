# 60 · B 端 host

本篇回答：B 上那个进程按什么顺序启动和退出；虚拟屏、A11y、注入、设置还原各要注意什么；哪些平台行为必须先探明。

host = 一个经 `adb shell` 用 app_process 启动的进程（shell 身份）：Kotlin 平台层 + 同进程加载的 Rust（rd + host-net）。

## 1. 启动与退出

启动顺序（顺序有意义）：

1. 读配置与上次遗留的“设置原值”文件。**有遗留就先还原**（上次是被杀掉的）。
2. 装未捕获异常处理器（§8）。
3. 起网络，生成或加载证书与 token，打印 URL。
4. 等 A 连接并 `Open{pkg}`。
5. 把要改的系统设置的原值写盘，然后改设置（§2）。
6. 建虚拟屏和采集（§3）。
7. 连 UiAutomation（§5）。
8. 在虚拟屏上启动 App（§4）。

退出（`Close`、断线超时、收到终止信号、崩溃处理器）：关 App 任务 → 释放虚拟屏 → 断开 UiAutomation → 还原设置 → 删除“设置原值”文件。

## 2. 全局设置

| 设置 | 会话中 | 备注 |
|---|---|---|
| 窗口动画缩放 | 0 | |
| 转场动画缩放 | 0 | |
| Animator 时长缩放 | 可调，默认 0 | 设为 0 时靠 Animator 驱动的转圈、进度动画会停住，个别界面会显得卡死，所以保留可调 |
| 窗口模糊 | 关 | |

- 原值**在修改之前**写到 `/data/local/tmp/hj/restore.json`。
- 还原发生在四处：正常退出、崩溃处理器、**独立看门狗子进程**（host 改设置前派生一行后台 `sh` 循环监视 `kill -0 <host_pid>`，若 host 被厂商后台管理 `kill -9` 无声杀掉，看门狗在 1–2 秒内立即读 `restore.json` 还原动画与模糊）、以及下次启动兜底。
- 只动上表列出的项。

## 3. 虚拟屏与采集

- 虚拟屏不带系统装饰：没有状态栏、通知、导航栏、桌面。尺寸与密度取 `Open.geom`，默认用 B 物理屏的宽和密度。
- 输入法不要显示在虚拟屏上，也不要弹到 B 的物理屏上：把该屏的输入法策略设为隐藏。文本用 `Text` 注入。
- 采集用 ImageReader（RGBA）。回调里只取最新一帧，JNI 侧就地直读平面（零拷贝），按行与上一帧 `memcmp` 只对变化行重算行 hash，记帧时间戳。缓冲多留一张，避免等空缓冲。
- B 可以熄屏：虚拟屏独立于物理屏，但设备可能进入休眠，host 要持有唤醒锁（§9 探针 P7）。
- 加高 / 缩回：对虚拟屏调 resize。之后的处理见 [30](30-scroll.md) §5。
- 受保护内容（帧是黑的）→ `Fault{BlankFrame}`。

## 4. 启动 App 与白名单

白名单（IDEA）：

```
com.android.settings  com.absinthe.libchecker  com.sirma.mobile.bible.android
de.szalkowski.activitylauncher  com.azefsw.audioconnect  com.android.calendar
com.miui.calculator  InfinityLoop1309.NewPipeEnhanced  org.qpython.qpy
com.and96.aggregator_news  com.aefyr.sai
```

- **动态白名单** = 上表普通包名 ∩ 本机已安装。随 `Welcome.apps` 下发（包名、显示名、图标）。
- 设备差异放配置文件里：例如 MIUI 的 `com.android.calendar` 在非 MIUI 设备上启动即崩，旧语料在那台设备上用 `com.google.android.calendar` 代替。
- 启动时用显式组件名（先解析出该包的 LAUNCHER Activity）。只给包名启动，有的 App 会解析失败。
- **跨包跳转策略与特殊白名单项（`[popup]`、`*`）**：
  - **默认（单应用严格模式）**：未配置扩展项时，栈顶只允许是当前 `Open.pkg`；跳到其他包或桌面 → 注入 BACK，报 `Fault{OffSession}`。
  - **多包白名单**：host 配置了多个允许跳转的包名时，允许在这些白名单包之间自由切换，但**不能退到桌面**（栈顶变成 Launcher 仍拦回或结束会话）。
  - **`[popup]` 项**：白名单包含 `'[popup]'` 时，允许任何包弹出的临时窗口/对话框（如系统权限框、分享面板、选打开方式、内嵌搜索浮层等非桌面覆盖层），关闭后回到原 App，不拦截。
  - **`*` 项**：白名单包含 `'*'` 时，允许任意切换所有 App，且**允许退回桌面**；在桌面点开或拉起的任何新 App 强制路由到**当前虚拟屏**上打开（`LaunchParams` / `displayId` 锁定虚拟屏，不跳到物理屏）。
- 退到桌面（未配 `*` 时）或任务消失 → 会话结束，`Fault{AppGone}`。

## 5. A11y

- shell 身份通过 UiAutomation 取节点树，连接时带“不压制其他无障碍服务”的标志（否则 B 上的读屏等服务会被停掉）。
- UiAutomation 同一时刻只能有一个持有者。host 运行期间不能再跑 `uiautomator dump`、Appium 这类工具。
- 要取的是虚拟屏上的窗口：按 displayId 过滤。
- 触发取树：窗口状态变化、窗口内容变化（去抖）、滚动结束。每次取树在 binder IPC 上耗时数十毫秒，异步执行不阻塞像素采集管线；每棵树绑上当时的**采集帧序号**与时间戳，由它推出的区域提案只对其后未发生内容变化的帧有效。
- 运行时每个节点只取需要的字段：类名、resource-id、屏幕矩形、是否可滚动及可滚方向、列表容器的 `CollectionInfo.rowCount` 与可见子项的 `CollectionItemInfo.rowIndex`、是否可编辑、是否有焦点、是否可见；文本只取窗口标题和顶栏标题。滚动事件取 `scrollY / maxScrollY` 及首末可见 item position。录制时取全量（[80](80-corpus-bench.md) §3）。
- A11y 总是晚于画面。所有用它的地方都要能在“还没到”（像素锚点先认页）和“到了但已过时”两种情况下工作（[20](20-pages.md) §2、§3）。
- 可编辑焦点变化 → `Focus{editable}`。

## 6. 输入注入

| 动作 | 怎么注入 |
|---|---|
| 点按、长按 | 触摸按下 / 抬起，目标 display = 虚拟屏 |
| `Touch` 流 | 按采样逐个注入，时间戳用 A 给的间隔 |
| 采集头滚动与远跳 | 鼠标滚轮精确像素公式（`(N + 0.5) / factor`，`SOURCE_MOUSE`）；远跳优先 A11y `ACTION_SCROLL_TO_POSITION`；回退为分步拖动（[30](30-scroll.md) §3） |
| `Wheel` | 原样注入滚轮 |
| 返回 | BACK 键 |
| 按键、文本 | 按键事件；文本用字符映射，映射不了的字符走“设剪贴板 + 粘贴” |
| 剪贴板 | `ClipSet` → 写 B 剪贴板；`ClipGet` → 读出回 `Clip` |

- 注入滚轮时，事件的设备 id 不要对应真实输入设备。否则较新的系统可能对滚轮也做惯性滚动。
- 滚轮注入可能让系统画出鼠标指针，采到的帧里不能有它（探针 P1）。
- 注入先唤醒虚拟屏；所有注入走同一个串行队列。
- **接收命令的循环不能睡**。耗时的手势（分步拖动）放到单独的手势线程，否则后面的输入全部排队。

## 7. 返回栈跟踪

- 需要知道：虚拟屏上那个任务的栈深、栈顶 Activity（包名 / 类名）。
- 首选任务栈变化的回调；拿不到就在每次输入之后查询一两次（旧项目用 `dumpsys activity`，一次几十毫秒）。
- 只反映 Activity 栈。同一个 Activity 里的 Fragment / Compose 导航不改变它，这类换页只靠 A11y 窗口事件和像素判断（[20](20-pages.md) §2）。
- 栈深为 1 且当前页不是模态页 → `Page.at_root = true`。

## 8. 进程健壮性（旧项目踩过的坑）

| 症状 | 原因 | 处理 |
|---|---|---|
| 任一线程抛异常，整个进程被杀 | 系统默认的未捕获异常处理 | 装自己的处理器：记日志、还原设置；能继续就继续 |
| host 无声消失 | 厂商的后台管理杀进程 | 独立 shell 看门狗进程立即还原系统设置；A 断线重连时给出提示 |
| 取输入管理器时空指针 | 隐藏接口需要系统上下文 | 通过系统上下文获取服务 |
| 采集目标的 display id 失效 | 虚拟屏被重建 | 失败时重新取 id |
| 往运行中实例的目录推新版本后崩溃 | 进程正映射着旧文件 | 推到新目录再切换，或先停再推 |
| `adb forward --remove-all` 把别的转发也删了 | — | 只删自己建的那条 |
| 端口被局域网其他机器连上 | 绑了 0.0.0.0 | 只绑 tailnet 网卡和回环 |
| `pkill -f` 匹配到自己 | 模式出现在自己的命令行里 | 按 PID 结束 |

## 9. M0 探针

每个探针是 host 的一个子命令，输出一份 JSON 结果，汇总进 `doc/probe-<设备>.md`。**不通过就走回退，并把结论写进该文件**，后续任务以它为准。P5 要等 M1 的传输做好后补上，其余在 M0 完成。

| # | 探什么 | 怎么判 | 不通过时 |
|---|---|---|---|
| P1 | 滚轮注入与远跳 | 白名单每个 App 的滚动容器：验证精确像素公式 `(N+0.5)/factor` 响应类型（精确 / 有动画 / 不支持）、无残留惯性、到边不回弹、无鼠标指针；测 `RecyclerView` 的 `ACTION_SCROLL_TO_POSITION` 远跳是否生效 | 有动画或不支持滚轮的容器用分步拖动；不支持动作远跳的走连续大步滚轮 |
| P2 | 虚拟屏上的 A11y 与 IPC 耗时 | 能取到虚拟屏窗口的节点树、`CollectionItemInfo.rowIndex` 和滚动事件；量出取树 IPC 耗时分布（数十 ms）与相对画面的延迟 | 该设备只做整屏 `Fixed`（没有滚动优化） |
| P3 | 超高虚拟屏与 `configChanges` | 每个 App：查 Activity 是否声明 `screenSize` 等 `configChanges`；测 resize 后多久出稳定帧、界面是否重建、滚动位置是否保留、最高能加到几倍、非列表部分有没有被拉伸 | 该 App `tall` 关（`tall` 默认本就关闭，只在活动切换边界改高度） |
| P4 | 输入法策略 | 聚焦文本框后，虚拟屏和物理屏都不弹输入法，`Text` 能输入 | 文本改走剪贴板粘贴 |
| P5 | 传输与 DERP 抖动（M1 任务 11 完成；JPEG 编码速度基准在 M0 本地探针测） | HTTPS 托管页（含 Chrome 本地网络访问授权）和 localhost 页都能用证书 hash 连上 B；测 Tailscale 直连与 DERP 中继各自的吞吐与 P95 抖动 | 必须修到通过；按 M0 实测的 JPEG 编码耗时与本探针的抖动定 `ahead_margin_ms` 与分片参数 |
| P6 | 栈回调 | 回调是否可用，和查询结果是否一致 | 用查询 |
| P7 | 熄屏 | 物理屏熄灭一段时间后注入输入仍有新帧 | 会话期间保持亮屏并调低亮度 |
| P8 | 设置还原（含看门狗） | `kill -9` 杀掉 host 后 2 秒内（不重启 host），看门狗已把动画与模糊还原到原值 | 必须修到通过 |

## 10. 命令行与调试口

- host 另开一个**只绑回环**的调试口，一行一条 JSON。本机经 `adb forward` 访问。
- 两类命令：
  - **直接驱动**：点某个节点、滚轮滚多远、返回、输入文本、取一帧 PNG、打印节点树摘要、跑探针。录制（[80](80-corpus-bench.md) §3）和手工调试用。
  - **扮演 A**：收发 wire 类型的 JSON 形式（`Tap`、`View`、`Back`…），和 A 发来的消息走同一条处理路径。场景的实跑模式用（[80](80-corpus-bench.md) §5）。
- 调试口不做鉴权，因此绝不能绑到回环以外的地址。

## 实现备注（M0）

- **调试口命令**（`host-adb/src/Debug.kt`）：`ping apps status open close quit frame frame_file tree stack tap long swipe path touch wheel back key text clipset clipget viewcfg scroll_to settle base mark step rec_start rec_stop jpeg_bench`。`open` 内部按 §1 顺序做：改设置 → 建虚拟屏 → 连 A11y → `force-stop` 后冷启动 App；启动时 `restore.json` 有遗留就先还原。
- **“等变化阈值再静止”**（人工录制与回放的默认等待，`Platform.waitSettle`）：注入命令带 `settle:{change, settle_ms, timeout_ms, ignore}` 时，注入后先等相对注入前的帧有 ≥ `change`×高度 行 hash 不同，再等 `settle_ms` 内没有新的行变化，最长 `timeout_ms`；返回 `changed / settled / tChange / tSettle`，超时未静止时附 `volatile:[y0,y1)`（最近 1s 一直在变的行带，两侧各留 4 行）。`ignore:[[y0,y1)…]` 里的行不计变化也不阻止静止。`base` + `settle{from_base:true}` 用于“先记基准、过一会儿再等”。
- **采集**：M0 先用 Kotlin 在 ImageReader 回调里拷贝最新帧并算行 hash（11 MB/帧 memcpy，实测可用）；**偏离了 §3 的“JNI 零拷贝直读”**。`libhj_host.so`（rd + host-net）目前只有 `Native.version()` 空壳，M1 把行 hash 和变化检测迁进 rd。虚拟屏标志位沿用旧项目验证过的组合（`PUBLIC|PRESENTATION|OWN_CONTENT_ONLY|SUPPORTS_TOUCH|ROTATES|DESTROY_ON_REMOVAL|TRUSTED|OWN_GROUP|UNLOCKED|NO_TOUCH_FEEDBACK|OWN_FOCUS|DEVICE_GROUP`，不含 `SHOULD_SHOW_SYSTEM_DECORATIONS`）。
- **输入法**：Android 13+ 没有系统装饰的虚拟屏，输入法策略默认回落到**物理屏**弹键盘。`open` 里用 `IWindowManager.setDisplayImePolicy(displayId, 2 /*HIDE*/)` 隐藏；文本 ASCII 走 `KeyCharacterMap` 逐键注入，其余整段“设剪贴板 + `KEYCODE_PASTE`”。
- **剪贴板**：`ClipboardManager` 必须用 `ShellContext` 自己构造（`ClipboardManager(Context, Handler)` 反射）；从系统上下文取出的实例调用包名是 `android`，被 AppOps 拒绝。
- **滚轮**：`wheel{x,y,v,h}` 注入 `SOURCE_MOUSE` 的 `ACTION_SCROLL`，`v/h` 单位是格。像素 = 格 × 容器实际因子 `F_eff`，**因容器而异**（`ViewConfiguration` 报 168；RecyclerView 实测一致，设置首页的 `ScrollView` 实测 ≈357）。所以精确公式写成 `v = -(N+0.5)/F_eff`，`F_eff` 由 `e2e/lib/wheel.mjs` 的 `calibrate`（一格量位移）或 P1 探针结果给出。
- **看门狗**：`sh` 脚本 `watchdog.sh` 每 0.5s `kill -0` host；读 `restore.json`（扁平 JSON，`null` = 原来没设置过 → delete）。踩过的坑：`sed` 输出末行无换行，`while read` 会丢掉最后一项（现在先补 `echo`）。
- **唤醒锁**：`open` 持有 `PARTIAL_WAKE_LOCK`，`close` 释放。探针 P7 受设备“充电时保持唤醒”影响，见 `probe-*.md`。
- **P3、P6**：M0 未做（P3 的 tall 默认关；P6 先全程用 `dumpsys` 查询）。P5 留给 M1 任务 11。
