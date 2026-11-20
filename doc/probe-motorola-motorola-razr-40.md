# 探针结果 · motorola-motorola-razr-40

由 `node e2e/probe.mjs` 产出（原始数据 `probe-motorola-motorola-razr-40.json`），`node e2e/probe-md.mjs` 渲染。不通过的项后续任务以“采用的回退”为准。

| 探针 | 结果 | 通过 / 不通过 | 采用的回退 |
| --- | --- | --- | --- |
| P1 滚轮与远跳 | 11 个 App，1 个精确（明细见下） | 部分通过 | 有动画 / 不支持 / 不精确的容器用分步拖动；不支持动作远跳的走连续大步滚轮 |
| P2 虚拟屏 A11y | 取树 IPC p50 3.0 ms / p95 4.0 ms（首次 86 ms）；取树期间出 3 帧；rowIndex 有；203 节点 | 通过 | 无需 |
| P3 超高虚拟屏 / configChanges | 未做：tall 默认关（GOAL §7 第 4 条），M4 任务 43 前不需要；需要 VirtualDisplay.resize 与采集 Surface 重建 | 未判定 | tall 关 |
| P4 输入法策略 | ASCII 走 keys，中文走 paste；输入框内容 ["wifi你好"]；虚拟屏 IME 策略设为 HIDE，之后 mInputShown=false | 文本可输入 | 无需 |
| P5 传输 / DERP 抖动 | 局域网 + localhost 页：RTT p50 7.2 / p95 24.4 ms，抖动(P95−P50) 17.2 ms，8 MB 单向流 0.54 s = 15151 KB/s。**未测**：Tailscale 直连 / DERP 中继：B 不在 tailnet（`tailscale status` 里没有它）；HTTPS 托管页 + Chrome 本地网络访问授权（Chrome 147+）。浏览器路径 verify=1 跑设置 / NewPipe：0 次校验失败；B 比 A 的时钟快 85 s 时，默认的 notBefore=now 证书被 Chrome 拒（Opening handshake failed）；改成 now-1d、有效期 13 天后通过 | 部分通过 | 证书 notBefore 取一天前（A/B 时钟差几分钟会让证书“还没生效”） |
| P9 `*` 退回桌面（M1 任务 15 新增） | `*` 白名单项的“退回桌面”：`am start --display N -a MAIN -c HOME` 只会把 intent 送给物理屏上已在运行的桌面（"delivered to currently running top-most instance"），虚拟屏上没有桌面；改用显式组件名 `am start --display N -n <HOME 组件> -f 0x10000000` 后桌面起在虚拟屏上，从桌面点开的 App 的栈也在虚拟屏上（实测：点相册后栈顶 com.google.android.apps.photos，display 同虚拟屏） | 通过（有回退） | 按 `cmd package resolve-activity` 解析出 HOME 组件显式启动 |
| P6 栈回调 | 未测回调；M0 全程用 `dumpsys` 查询（几十 ms，按需调用） | 未判定 | 用查询 |
| P7 熄屏 | mStayOn=true 表示设备正在充电且开着“充电时保持唤醒”，熄屏键被系统忽略；不得改表外设置，需拔电或用户关掉该开发者选项后再测 mWakefulness=Awake | 未判定：物理屏没能熄灭 | — |
| P8 设置还原（含看门狗） | kill -9 后 572 ms 内四项设置回到原值（含 adb 往返）；原值 {"window_animation_scale":"1.0","transition_animation_scale":"1.0","animator_duration_scale":"1.0","disable_window_blurs":"0"} | 通过 | — |
| JPEG 编码速度（B） | q50: 18.7 ms/MP（95967 B）；q70: 18.8 ms/MP（114598 B）；q90: 19.1 ms/MP（163167 B）。B 端 Bitmap.compress(JPEG)，1080x2640 整屏，中位数；设置首页（文字为主） | 已量 | 用于定 `ahead_margin_ms` 与分片参数 |

## P1 明细

| App | 容器 | F_eff（像素/格） | 公式 (N+0.5)/F_eff 的 1/5/37/120/333 像素 | 结论 | 到边 | 远跳 |
| --- | --- | --- | --- | --- | --- | --- |
| com.azefsw.audioconnect | RecyclerView  | — | — | 不支持滚轮（v=-1 无位移） | — | — |
| com.miui.calculator | — | — | — | 无竖直滚动容器 | — | — |
| com.android.settings | ScrollView com.android.settings:id/settings_homepage_container | 357 | 0 / 4 / 36 / 119 / 332 | 近似（0/5 精确） | 无 | — |
| com.absinthe.libchecker | RecyclerView  | 168 | 1 / 5 / 37 / 120 / 333 | 精确 | 有位移（回弹/未到底） | — |
| com.sirma.mobile.bible.android | — | — | — | 无竖直滚动容器 | — | — |
| de.szalkowski.activitylauncher | ScrollView de.szalkowski.activitylauncher:id/content_main | 168 | 0 / 0 / 0 / 188 / 333 | 近似（1/5 精确） | 有位移（回弹/未到底） | — |
| com.android.calendar | — | — | — | 无竖直滚动容器 | — | — |
| InfinityLoop1309.NewPipeEnhanced | — | — | — | 无竖直滚动容器 | — | — |
| org.qpython.qpy | — | — | — | 无竖直滚动容器 | — | — |
| com.and96.aggregator_news | — | — | — | 无竖直滚动容器 | — | — |
| com.aefyr.sai | — | — | — | 无竖直滚动容器 | — | — |
