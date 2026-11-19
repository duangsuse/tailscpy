# info.md · 思维链里漏掉的项目功能 / 算法优化

> 来源：`old/llm-think.txt`（生成 `GOAL.md` + `doc/*.md` 的那轮推理记录）与当前文档逐条比对后的缺口清单。
> 每条给出：是什么、依据（transcript 行号）、与现有文档的关系、优先级。
> "现有文档"指 `GOAL.md`、`IDEA.md`、`doc/10`–`doc/90`、`old/*.md`。
>
> 编号说明：编号沿用发现时的流水号，因此缺 5 与 40 —— 原第 5 条（页面身份像素为主）后来作为第 30 条写入；
> 原第 40 条（白名单越界归因 / `OffSession`）已并入第 14 条，不再单列。

---

## 1. A11y 绝对锚：用条目下标 + `ACTION_SCROLL_TO_POSITION` 定位画布

**是什么**

- RecyclerView / ListView 每个可见 item 的 A11y 节点带 `CollectionItemInfo.rowIndex`，就是 adapter position（绝对、无漂移）；LinearLayoutManager 的滚动事件还带首/末可见 position（fromIndex / toIndex），item 总数也可拿到。
- ScrollView / NestedScrollView 的滚动事件直接给绝对 `scrollY` / `maxScrollY`；RecyclerView 的 `scrollY` 恒为 0。
- 因此画布可以按 `(item index, offset within item)` 寻址，而不是靠累计像素位移：重进页面时位置不同、条目高度不等、列表中间插入，地图都保持稳定（插入表现为索引位移）。
- `ACTION_SCROLL_TO_POSITION`（ListView 与 RecyclerView 都接受）可以让 B 把采集头零偏移跳到指定条目下标 → 滚动条远跳、点屏幕外的条目，都不必靠连续步进。
- 配套事实：`TYPE_VIEW_SCROLLED` 被框架节流到约 100 ms 一次并累加 delta，所以与 A11y 的交叉校验只能在**页面停稳后**做，不能逐帧比。

**依据**

`old/llm-think.txt:2253-2258`（"the default LayoutManager sets that from the item's adapter position. That would let me anchor the map on (item index, offset within the item) rather than relying on accumulated pixel deltas"）、`:5119-5122`、`:5610-5612`、`:7177-7178`、`:8054`、`:8455`、`:9179-9183`

**与现有文档的关系**

- `doc/40-codec.md` §3 只写"与 A11y 滚动量交叉校验"；`doc/20-pages.md` §4 的内容坐标是纯像素 y，重进页靠行 hash 对齐重求采集头 → 没有 item index 这一层。
- `doc/60-host.md` §5 的运行时 A11y 字段清单里没有行号 / item count / 首末可见 position / 滚动动作。
- `doc/30-scroll.md` §3 的"远距离跳转"只有"连续步进"。
- `old/llm-goal1.md` §5 风险 5 写"RecyclerView 无绝对偏移：只有 delta，累计会漂移"——**这条前提是错的**：无绝对偏移的是滚动 delta，不是条目下标。`doc/` 继承了它。
- `doc/80-corpus-bench.md` §3 的 `a11y_event` 只写"滚动量等"，录制格式需要新增 collection info 字段（uiautomator 的 XML dump 会丢掉它）。

**优先级**：HIGH —— 推翻一条既有前提，同时改地图寻址模型、录制字段和探针项。

---

## 2. 列带行 hash + 每格有效位

**是什么**

- 行 hash 不覆盖整行宽度，而是按 4–8 个列带（每带 64 / 128 px）分别算，整行 hash 是列带 hash 的折叠。
- 地图单元从"一行"变成 `(row, band)` 对，每个带各带一个有效位 → FAB 压住的那一带可以单独标为"未知 / 固定"，不废掉整行；右缘滚动条另占一个窄带。
- 好处：脏区在二维上定位；横向偏移将来也可按列带投票。
- 投票权重改为"该行 hash 在上一帧中的**稀有度**"（逆频率加权），周期性分隔行、重复图案因此几乎不投票，比单看颜色游程数更能压住假峰。
- 每行另附一份软签名（游程数、平均亮度、若干列带和）。精确 hash 匹配被破坏时（越界回弹、压暗、alpha 淡入淡出），用软签名做 1D 互相关投票兜底。

**依据**

`old/llm-think.txt:289-290`（"fixed column bands for the content map, about eight of them plus a narrow one for the scrollbar at the right edge, so each cell is a row-and-band pair with its own validity bit"）、`:563-567`、`:1145-1146`、`:1488`、`:1749-1750`、`:2775-2779`、`:4998`（"weighting each row by how rare its hash is in the frame"）

**与现有文档的关系**

- `doc/40-codec.md` §2 规定"每个区域每行一个 64 位 hash，覆盖区域宽度"，信息量 = 颜色游程数 → 与本条相反。
- `doc/40-codec.md` §12 只把"按列带的引用（图标列、文本列分别匹配）"列为消融候选项，那是**引用匹配**，不是行 hash / 位移检测的基本单位。
- `doc/30-scroll.md` §8 处理 FAB 遮挡的方式是"被挡住的画布行这一帧不写"（丢整行），本条是"那一带标未知、其余带照常验证"。
- `doc/40-codec.md` §11 对重复图案是**放弃投票**，本条用逆频率权重 + 软签名兜底，保留投票能力。

**优先级**：HIGH —— 直接改 §2 的行 hash 定义，且能救回 §11 里两个只能放弃的场景。

---

## 3. 信号处理识别扩展（正对"只用基础信号处理"这个前提）

**是什么**

在行 hash 投票之外补一层经典信号处理：

| 用途 | 方法 |
|---|---|
| 位移估计（行 hash 只认完全相等，估不准时兜底） | 竖直投影剖面 + FFT 互相关，再用行 hash 确认 |
| 列表条目间距 pitch | 行签名序列自相关取峰；间距均匀时内容坐标 = index × pitch，地图天然精确，内容总高 = 项数 × pitch，滚动条滑块位置可精确换算为内容偏移 |
| 滚动条窄带宽度 | 运动补偿后残差的**列剖面**（不是固定裁 40 px） |
| 滚动区边界 | "随 dy 一起移动的最长行段" |
| 固定元素（吸顶、FAB） | 位移补偿后逐列 / 逐行失配，标签平滑成区域 |
| 非刚性拉伸 | 位移随 y 变化 → 该区不建地图 |
| 周期性假峰消歧 | 投票直方图出现 pitch 整数倍的梳状峰时，用已知注入位移 + 自相关峰距联合判定 |

**依据**

`old/llm-think.txt:442-449`（"vertical projection profiles and cross-correlation via FFT to estimate the scroll offset, then confirming with row hashes"）、`:1752-1757`、`:3045-3051`、`:3935-3939`、`:4302-4310`

**与现有文档的关系**

- `doc/40-codec.md` §3 只有"已知预期位移先验证、否则投票"，没有互相关、没有 pitch 估计、没有列残差分区。
- `doc/40-codec.md` §11 对"等高且内容相同的条目"只能"投票不成立 → 按无位移处理"；pitch 自相关给出的是消歧手段而不是放弃。
- `doc/30-scroll.md` §7 的滚动条窄带宽度是参数 `sb_w`（"取系统滚动条宽度"），没有"从残差列剖面量出来"的路径。

**优先级**：HIGH —— 这是 IDEA / 用户前提"只用基础信号处理做识别"目前最缺的一块，且每条都是可核算的原语。

---

## 4. 内容定义切块 + 内容寻址条带 + manifest 作页版本

**是什么**

- 长页不按固定网格切，按**内容边界**切：在条目之间的空白分隔行处断开（阈值约 8 px 空白行游程，按密度 / DPI 可调；A11y 的条目边界作先验），无空白行时回退到约 256 行的固定切。上限 256 行。
- 好处：同一条目无论出现在哪个 y 都得到同一个 hash → 跨位置、跨页面、跨重进都能去重；顶部插入一张 banner 不会让下面所有行 hash 全变；列表中间插入一项，只改 manifest 里的 y 位置，不重发像素。
- 页面 = **内容寻址条带的有序列表 + 一个 layout**。插入 / 删除 / 重排只改 layout，不改像素。
- manifest 的 hash 就是页版本（两端各自重算，不会像计数器那样漂移）；版本不一致时**按条带比对**，只重发 A 最后确认版本之后写过的条带；B 丢了状态时 A 在握手时上报条带 hash 清单即可重建，B 不必持久化。
- A 侧是内容寻址的块库，B 发条带清单，A 回**缺块位图**。推论：`cache_rev` / `pg_rev` 退化为"快路径"，A 可以自由淘汰块而不会让两端失步。
- 落盘的条带必须**自包含**（不依赖 zstd 上下文与跨条带 IBC 引用）：线上协议保留跨条带上下文，落盘由 A 在空闲 worker 用同一个 crate 的自包含 intra 模式重编码。

**依据**

`old/llm-think.txt:593-604`（"content-addressed chunks keyed by hash… A can evict chunks freely without breaking the sync"）、`:1295`、`:1409-1421`、`:2781-2785`、`:3470-3471`、`:5948-5949`、`:7212-7219`、`:2547-2548`、`:5152-5165`

**与现有文档的关系**

- `doc/40-codec.md` §2 明确选了相反路线："行是最小单位，不依赖块大小，所以高密度屏、密集列表不需要特殊处理"。
- `doc/70-viewer.md` §6 的 OPFS 布局按**固定**行数切分块，不是内容定义。
- `doc/20-pages.md` §7 的版本模型是页级计数器 `pg_rev` + `base`/`pending`，失配就 `Purge` 整页；`doc/50-protocol.md` §3 的 `Inventory{pages: Vec<(PageId, PgRev)>}` 也是页粒度 → 没有 manifest、条带级版本、缺块位图。
- `doc/40-codec.md` §12 把"页内 zstd 长窗口"列为候选，但没写它对淘汰粒度和跨会话的约束。

**优先级**：HIGH —— 协议与缓存的核心粒度问题；300 KB/s 下"整页重置"代价很高。

---

## 6. 预取是码率控制问题，不只是调度问题

**是什么**

一旦 `滚速 × 每像素字节 > 链路速率`，再多提前量也追不上，只能露白块。对应的控制律：

- 算出**最大可持续滚速**：300 KB/s、文字列表约 42–58 B/行 → 约 5000 px/s ≈ 2 屏/s；图标密集列表掉到 1 屏/s 以下。
- 供给不足时不排队，而是走**保真度降级阶梯**：先发半分辨率 / 模糊预览，管线空闲时再锐化。
- 视口滚到"已收到内容的边缘"时用**软墙**减速，并给部分 tile 填区域底色，而不是直接露白块。
- 指标上加一条"软墙卡住用户的时长"——它比白块率更能说明预取是否跟得上。
- 两遍预取：先发便宜的文本层（约 20–40 B/行）把预取前沿推出去，重内容随后补。

**依据**

`old/llm-think.txt:5065-5072`（"if scroll speed times bytes per pixel exceeds the link rate, no lead helps and blank blocks appear… the prefetch needs to be treated as a rate-control problem"）、`:3647-3653`、`:6093-6097`、`:6799-6800`、`:7130-7131`

**与现有文档的关系**

- `doc/30-scroll.md` §4 的提前量公式 `ahead_px = |vy| ×（可绘制耗时 P95 + ahead_margin_ms）` **不含带宽项**；`doc/10-dataflow.md` §6 只限了在途字节上限。
- `doc/30-scroll.md` §2 的既定行为是"允许滚进未知方向 + 画占位"，没有"在已知前沿处减速"。
- `doc/80-corpus-bench.md` §6 有白块指标，没有软墙时长。

**优先级**：HIGH

---

## 7. ScrollCapture：超高虚拟屏之外的第三条预取后端

**是什么**

Android 隐藏接口 `requestScrollCapture`（SystemUI 长截屏那条路）能让框架把滚动容器的**屏外内容**渲染进我们提供的 Surface。期间 helper 临时关掉 overscroll 与垂直滚动条，结束后恢复滚动位置。

优点：

- 不改虚拟屏几何 → 不触发配置变更、不重建 Activity、不丢滚动位置。
- 拿到的是**精确内容坐标**的条带。
- 只画该 View（不画整个窗口），FAB / 底栏天然不参与。

约束：

- 只支持 ScrollView 类 / ListView / RecyclerView 类 / WebView；App 可以自带 capture callback。
- 需要 `READ_FRAME_BUFFER`（shell manifest 已声明）。
- 捕获期间视图**真的在滚动** → 注入点击前必须先结束 capture session。
- 隐藏 API 在 app_process / OEM 上是否可用 → 要探（适合落成 `doc/60-host.md` §9 的一个新探针）。
- 透明 RecyclerView 的窗口背景可能被漏掉。

建议的形态：`Prefetcher` 有三种后端，按区域逐个尝试；1× 滚轮采集头作为基线先做。

**依据**

`old/llm-think.txt:5090-5111`、`:5410-5417`、`:5506-5516`、`:5629-5633`、`:7352-7356`、`:7791-7795`、`:7949-7950`、`:8936-8940`

**与现有文档的关系**

- `doc/30-scroll.md` §5 只有"超高虚拟屏"这一条预取后端。
- `doc/60-host.md` §9 探针表 P1–P8 没有对应项；`doc/60-host.md` §3 只讲 ImageReader 采集。

**优先级**：HIGH（但需先探可用性）

---

## 8. 调色板必须降到条带级 + token 流 + 两级种子表

**是什么**

语料实测：**整屏**色数中位 636（razr）/ 300（xiaomi），≤256 色的帧只有 8% / 32% → "整屏一个 8-bit 调色板"在真实 App 上不成立。规则：

- 每条带用**局部**调色板，≤256 项（索引正好 1 字节）；超出回退残差编码，rare 色走 escape 像素。
- 索引不是序数关系，PNG 式预测滤波会让它变大 → 正确做法是**按亮度排序**调色板，把索引平面编成行内 RUN 与 `COPY_ABOVE` token 流再过 zstd（zstd 顺带匹配跨条带的重复字形行）。
- 两级结构：App 级**种子表**（主色、表面色变体、文字色）随缓存持久化；每条带的调色板只存相对种子表的增量，纯色填充可直接指向种子索引。
- 旧文档的"≤16 色"阈值被否掉：HEVC-SCC 的 palette 上限是 63 项 + escape，而抗锯齿文字的灰阶远多于 16，16 太紧。
- 分类阈值（多少色算纯色块 / 文字块）按实测 regret 定，不写死。

**依据**

`old/llm-think.txt:7086-7093`、`:7435-7436`、`:7529`、`:7860`、`:8056`、`:8060-8067`、`:6205-6209`、`:6554-6558`

**与现有文档的关系**

- `doc/40-codec.md` §6 只写"矩形内调色板 + 索引平面，再过 zstd" → 没有容量上限、没有超限回退、没有 token 层、没有亮度排序理由。
- `doc/40-codec.md` §12 只把"页级共享调色板"列为消融候选，没有两级结构与持久化规则。
- `doc/50-protocol.md` §6 参数表里没有 palette 容量 / 分类阈值这两个 key。

**优先级**：HIGH

---

## 9. 压缩强度按链路 backlog 自适应（旧结论在新链路上是反的）

**是什么**

旧项目的判据是局域网，"WebP 无损太慢不能用"。在 300–800 KB/s 上要重算：

- 整屏文字帧 WebP-lossless 编码约 196 ms，而省下的 ≈80 KB 在 300 KB/s 上相当于 ≈267 ms 线上时间 → **编码耗时可以被传输节省摊平**，盈亏平衡点约 440–450 KB/s，而目标链路只有 300 KB/s。
- 配套：按条带**并行编码**（手机 8 核，单核 196 ms → 25–40 ms），编码与发送流水重叠。粗算：每 1 ms 编码时间值约 300 字节流量。
- zstd 同理：157 KB 文本页 zstd-3 → zstd-9 降到 126 KB，多花 20–50 ms 换 ~20% 字节是划算的 → `zstd_level` 应随发送队列自适应，而不是固定沿用旧项目的 3。
- 同类思路：每 App 训练一个 zstd 字典（作用在索引平面上），可覆盖从未见过的文字；但要先测 zstd repcode 命中率，只有再省 ≥10% 才值得建。

**依据**

`old/llm-think.txt:1983-1993`（"at 300KB/s, saving about 80KB is roughly 267ms on the wire, which is more than the 196ms it takes to encode a full text screen"）、`:2587-2597`、`:3667-3668`、`:610-616`

**与现有文档的关系**

- `doc/50-protocol.md` §6 参数表写 `zstd_level` 起步值"沿用旧项目的 3"，且按 App 自适应项里写"图片多的 App 压缩级别**调低**，省编码时间" → **在新链路上方向相反**。
- `old/10-codec-modules.md` 保留了旧的"WebP 无损不可用"结论，transcript 已明确要重估。

**优先级**：HIGH —— 当前参数表的默认规则在新链路上是错的。

---

## 10. 有损图片两档阶梯 + 首屏交付顺序

**是什么**

- 图片编码只做两级：先半尺寸或 q≈35 的**预览**，再目标质量；精修排队进预取优先级，条件是目标质量字节估算仍高于阈值。
- **页内字节顺序**固定：底色 / 骨架 solid rect（让点击立刻有反馈）→ 文字条带自上而下 → 图片（先预览后终稿）。
- 远处预取的图片只发低质量；滚到可见且管线空闲时**整块替换**为全质量（整块替换，不需要精确增量）。
- 色度采样按区域选：4:2:0 会糊掉图内的彩色细边，小图标可能必须 4:4:4，照片可留 4:2:0。

**依据**

`old/llm-think.txt:2390-2398`、`:2921-2923`、`:6533-6537`、`:6055-6056`、`:3357-3358`

**与现有文档的关系**

- `doc/40-codec.md` §7 只有单档 `Image` 有损 + `img_q` + 掩码 → 没有质量阶梯、没有精修 op、没有采样率选择。
- `doc/10-dataflow.md` §6 只区分"可见区优先"，没有页内交付顺序。

**优先级**：HIGH

---

## 11. 阴影是"位置相关像素" → 阴影单列为一个图层，允许非 0px

**是什么**

Material / M2 风格的 elevation shadow 是按**行在屏幕上的位置**渲染的：光源相对屏幕固定，同一段内容在两个滚动位置上像素不同。这会破坏整行行 hash 的精确匹配，bg 层的无损保证因此被挑战。

**处理（已拍板）**：阴影**单独作为一层**，这一层允许非 0px。也就是说：

- 阴影像素不参与行 hash 的精确匹配（否则滚动一格整卡行的 hash 全变），也不进入"逐像素一致"的校验范围。
- 阴影层可以按有损 / 近似方式编码与合成，不承担 0px 承诺。
- 其余层（文字、symbolic 图标、无损布局）仍保持 0px；校验掩码需新增"阴影层"这一类跳过区（现有掩码已有 `Image` 置位、`Fill`/`Lit` 清零、`Copy` 跟随的机制，阴影层按同类处理）。

需要配套确认的事实：M3 的 card 基本已去掉 spot shadow，HyperOS / AOSP 设置较平，但**位置相关像素在语料里的占比**仍要在 M0 量一次（决定这一层的实际收益与容差要求）。

**依据**

`old/llm-think.txt:6494-6516`（"elevation shadows shift with each row's position on screen, so card edges don't stay identical when scrolled. That would break exact row-hash matching on full-width rows"；"if elevation shadows shift with each row's screen position, the background layer can't stay exact under scrolling"；"Material 3 cards mostly skip the spot shadow now… OEM skins like HyperOS and AOSP's Settings are flat enough"）

**与现有文档的关系**

- `doc/40-codec.md` §6 把阴影归为"渐变 / 阴影 / 颜色多但不是照片"→ `Lit{Residual}`，即仍按无损处理；§2 的行 hash 覆盖整行宽度。
- `doc/40-codec.md` §10 的校验跳过项只有"有损图片、模态页下面压暗的背景"，没有阴影层。
- IDEA 的"其他层 0px 还原"需要按这条打一个例外。

**优先级**：HIGH —— 影响"0px"承诺的适用范围与校验掩码设计。

---

## 12. Dim（模态压暗）用 3×256 逐通道 LUT 精确表达

**是什么**

固定 scrim 颜色 + 固定 alpha 的混合是**逐通道的点到点映射**（黑色 scrim 时输出就是输入的定比缩放），因此每通道一张 256 项表就能精确描述，delta 编码后三张表约一百字节（推算，未实测）。要点：

- 压暗必须在**合成时**施加，不能烘进画布（否则关掉弹窗后底图已被压暗，无法还原）。
- 若合成器用了 dithering 或跨通道混合，这个"逐通道函数"性质被破坏 → 回退到稀疏残差列表。

另一条相反方向的量化：如果改成"两端跑同一个整数混合公式 + 稀疏残差保 0px"，代价约 0.2 bit/px、2.6 MP 下约 **65 KB**，与整页重发接近 → 应做成可调项（近似模式默认关），而不是默默默认。

**依据**

`old/llm-think.txt:8231-8241`（"for each channel value, record what it turns into after the dim, and send that as a 768-byte lookup table… as long as the blend is a per-channel function without dithering"）、`:8946-8951`、`:5021-5030`（"The dimming then has to be applied at view time rather than baked into the cached tiles"）、`:7392-7399`

**与现有文档的关系**

- `doc/20-pages.md` §6 的模态页 `dim` 只是一个 `Rgba` 颜色，由 B 从像素求出；`doc/50-protocol.md` §2 的 `Modal{rect, dim: Rgba}` 同。
- `doc/40-codec.md` §10 把压暗背景整体排除在校验之外 → 现有文档是"放弃精确性"，本条是"用 LUT 把它变回精确"，且成本极低。

**优先级**：HIGH —— 一条协议字段的量级变更（`Rgba` → 逐通道 LUT）。

---

## 13. 悬停 / 半透明浮层的解混：两背景解 alpha

**是什么**

FAB 这类压在滚动内容上的悬浮按钮，其像素与背景混合，按 A11y 矩形直接切出来会带错底色、边缘出现色晕。做法：

- 取两次不同滚动位置的采集，让同一元素分别覆盖在两个已知背景 B1、B2 上，由 C1、C2 解出每像素的 alpha 与颜色。
- 滚动停下后，采集位置与视口对齐，用真实帧替换合成结果（合成只是过渡态）。

另一类：HyperOS 风格的半透明模糊顶 / 底栏**不是静态固定元素**，matting 无解 → 只能当作 Overlay，只在滚动停稳时刷新，运动中接受轻微过期。前提认知：IDEA 的"关模糊"只覆盖 window 级模糊，挡不住 App 内的 RenderEffect 模糊。

**依据**

`old/llm-think.txt:2906-2916`（"two captures at different scroll offsets give C1 and C2 over backgrounds B1 and B2, so the difference yields the overlay's alpha… Once scrolling stops, the capture lines up with the viewport and the synthesized composite gets replaced by the real frame"）、`:2580-2584`

**与现有文档的关系**

- `doc/30-scroll.md` §8 对 FAB / 吸顶标题只有"切成压在上面的 `Fixed` 区域，被它挡住的画布行这一帧不写"的退让方案，没有解混方案。
- `doc/60-host.md` §2 的全局设置表里有"窗口模糊 关"，但没有"App 内 RenderEffect 模糊关不掉"这个前提。

**优先级**：HIGH —— 决定 FAB / 模糊栏是"能 0px"还是"只能近似"。

---

## 14. 语料与 bench 的量化口径（几个可直接当验收线的数字）

**是什么**

- **首屏预算**：300 KB/s × 300 ms ≈ **90 KB**（p90 目标）。文字页无损 71–157 KB → 2–4 屏/s；照片页 0.7–2.3 MB → 0.13–0.45 屏/s。首屏和换页才是延迟目标，不是滚动。
- **采集头上限**：1× 采集头约 **57,600 px/s**（0.8 屏/步 × 30 步/s，仅上界）。文本列表的瓶颈不是带宽而是 App 的渲染 / 采集速率（60 fps）。
- **加高屏覆盖率**：402 个 App 内 dump 的滚动区形态 → 约 32% 无滚动区、33% 单区贴底、16% 单区带底栏、18% 多区（多区里 42/71 是嵌套）→ **加高屏合格页只覆盖约一半**，采集头扫描必须作为并行路径存在。
- **白名单归因被推翻**：451 个 dump 里 49 个（11%）根包名不是目标 App，但其中 30 个是**另一个白名单 App 在前台**（采集脚本没把目标 App 拉回前台，属标签错位），真越界只有 19 个（≈4.2%）→ 越界码应从 `OffWhitelist` 改为 `OffSession`（判据是"前台包名 == 会话 App"）。会话身份还应绑到**任务 / 根 Activity** 而不是栈顶包名（设置会合法地拉起别的包的子 Activity）。
- **旧字节口径不可直接用**：旧语料 `back` 占下行 48.6%，但这些帧多数是 App 退到桌面后的**桌面壁纸**，单 App 会话根本不会产生 → 容量预算前必须按"栈顶包名 != 会话 App"打标剔除后重算（旧 `back` 帧里 21/87 超过 2 万色，最大的一帧就是壁纸）。

**依据**

`old/llm-think.txt:1210-1211`、`:1462-1463`、`:8096-8108`、`:8372-8375`、`:8572-8579`、`:8640-8644`、`:8806-8815`、`:8974-8977`、`:4050-4051`、`:4584-4586`

**与现有文档的关系**

- `doc/80-corpus-bench.md` §6 定义了指标和网络档位，但**没有字节预算、没有 time-to-first-paint、没有每屏字节 → 屏/秒 → 采集头 px/s 的换算**。
- `doc/80-corpus-bench.md` §2 有"越界帧打标并剔除"和"`app` 字段不是实际前台"两条规则，但**没有量化结论**（11% 里只有 4.2% 是真越界）。
- `doc/50-protocol.md` §2 的 `Fault.code` 仍是 `OffWhitelist`。

**优先级**：HIGH

---

## 15. 三个缺失的 M0 探针 + 语料必须重建

**是什么**

当前探针表 P1–P8 里缺三项，都是设计参数的前置条件：

1. **手机端 JPEG 编码速度** —— 决定有损图片能否实时。libjpeg-turbo + NEON 估 50–100 MP/s，整屏 2.6 MP 约 30–50 ms（需实测）。
2. **Tailscale 直连 vs DERP 中继**各自的速率与抖动 —— 决定提前量与预算档位；DERP 的 P95 抖动大，余量参数要自适应。
3. **在虚拟屏上取 A11y 树的耗时** —— 几十毫秒级 binder IPC，决定树快照能否异步化而不拖慢像素管线。

另：旧语料约 48.6% 的下行字节是退到桌面的壁纸帧，单 App 会话不会产生 → **语料必须重录**，旧数字不能直接用于预算。

**依据**

`old/llm-think.txt:1563-1570`（"gauge how expensive it is to fetch the accessibility tree on a virtual display… JPEG encode speed on the device… the tailscale link's direct versus relayed rate and jitter"）、`:3404`、`:3667-3668`、`:5407-5408`

**与现有文档的关系**

- `doc/60-host.md` §9 的探针表 P1–P8 含熄屏（P7）、IME（P4），但没有这三项。
- `doc/80-corpus-bench.md` §2 说要重录，但没给"旧字节里有多大比例不可用"的量级。

**优先级**：HIGH

---

## 16. 滚轮注入的精确像素公式

**是什么**

框架把 `AXIS_VSCROLL` 轴值乘 `getScaledVerticalScrollFactor()` 后取整成像素：

- 默认垂直滚动因子 = **64dp × density**（420dpi → **168 px / 轴单位**）；NestedScrollView 用 theme 的 list item height，基准不同 → 因子**按容器类**不同。
- 要精确位移 N 像素，轴值取 `(N + 0.5) / factor`（RecyclerView 用 int 截断，+0.5 能同时命中"截断"和"取整"两种实现）；也可以给轴值加 0.25 偏置达到同样目的。
- 配合 `SOURCE_MOUSE`：不触发拉伸回弹，RecyclerView 也不触发 fling → 一次滚轮事件 = 确定的一步。

这比现在"第一次滚动时校准一格多少像素"更准（校准是经验值，公式是闭式）。

**探针**：Compose 的 `Modifier.scrollable` 可能对滚轮做动画 → 每页入口注入一个已知像素量，看像素投票判定响应属于哪一类：**精确 / 有动画 / 不支持**，再决定用滚轮还是零末速拖动。现在 P1 是按 App 的主列表测一次，不是按页 / 按容器。

**依据**

`old/llm-think.txt:3964-3969`（"AXIS_VSCROLL is a float… an exact pixel offset is just the desired delta divided by the factor… skips overscroll stretch and, for RecyclerView, fling"）、`:3691-3692`、`:5211-5218`（"the default vertical scroll factor is 64dp scaled by density, which at 420dpi comes to 168px per axis unit"）、`:2271-2272`

**与现有文档的关系**

- `doc/30-scroll.md` §3 只有"一格滚多少像素因容器而异。每个区域第一次滚动时用实际像素位移校准"——是经验标定，没有闭式换算与取整对齐规则。
- `doc/60-host.md` §6 写了"滚轮事件的设备 id 不要对应真实输入设备"（防惯性），但没有因子常量与精确命中公式；§9 的 P1 只测"位移可重复"。

**优先级**：HIGH

---

## 17. 128 位持久内容 hash（64 位只用于帧内校验）

**是什么**

- 64 位行 hash 在约 10⁷ 条带上的生日界约 3e-6 → 帧内使用可忽略。
- 但**跨会话的持久引用**不行：B 无法对 A 存下来的像素逐字节比对，只能信 hash → 持久条带缓存用 **128 位**内容 hash。
- 帧内校验（两端都持有像素、可直接 memcmp）用 64 位即可。

**依据**

`old/llm-think.txt:7187-7190`（"128-bit content hashes for the persistent strip cache, since B can't pixel-compare against A's stored references and has to trust the hash alone. A 64-bit hash is fine for in-frame checks where B can memcmp the pixels directly"）

**与现有文档的关系**

- `doc/40-codec.md` §2 只规定"每个区域每行一个 64 位 hash"；`doc/20-pages.md` §7 的持久化版本对齐没有涉及 hash 位宽。
- 这条与第 4 条（内容寻址条带）配套才成立。

**优先级**：MEDIUM

---

## 18. 视口位置走 QUIC 数据报（latest-wins 绝对值），最终位置走可靠流

**是什么**

- A→B 的视口位置是**绝对值**（`View{region, y}`），后一条覆盖前一条，丢一条没有代价 → 可以 30–60 Hz 走**不可靠数据报**，高频视口更新不会排队在后面。
- 手势结束时的**最终静止位置**必须走可靠流。
- 尺寸：Tailscale 1280 MTU → IPv6 下 UDP 载荷约 1232 B，扣掉 QUIC 头 / AEAD / WebTransport（HTTP/3 session id）开销后约 **1100 B**；应运行时读 `maxDatagramSize()`，不写死。

**依据**

`old/llm-think.txt:2414-2415`（"scroll positions can go as absolute, idempotent ScrollTo values at 30–60Hz over datagrams, since the viewer composites scrolling locally"）、`:2736-2737`（"Latest-wins absolute values… but the final resting position has to go on a reliable stream"）、`:5576-5583`

**与现有文档的关系**

- `doc/50-protocol.md` §5 明确写"**暂不使用 QUIC 数据报**；以后要用，单个数据报也要守住这个上限" → 本条把"以后"具体化：哪条消息走、什么语义、哪条必须可靠。
- `doc/10-dataflow.md` §6 的"每个渲染帧最多发一条 `View`"已经暗示了 latest-wins 语义，但没有落到传输层。

**优先级**：MEDIUM

---

## 19. 乐观"前进"导航（导航边表）

**是什么**

在"乐观返回"（Back 方向）之上再往前一步：

- 记录"点了某个节点 → 进了哪个子页"的**导航边**（被点条目的稳定键 = A11y text + resource-id）。
- A 在点击瞬间直接显示缓存的子页，B 随后验证 / 纠正。
- 风险是误点或画面闪回 → **按 App 可选、只对至少出现过两次的边启用、默认先不开**（B 不能无副作用地替用户预访问页面）。
- 配套：同一页面桶内的变体可用首屏稀有行 hash 的 **MinHash sketch** 挑选，不必依赖标题文本。

**依据**

`old/llm-think.txt:3758-3759`（"the parent page key combined with the tapped item's stable key… showing the predicted page only once that edge has been validated a couple of times"）、`:5170-5176`、`:5303-5304`、`:6225-6226`、`:6822-6823`、`:1329-1333`

**与现有文档的关系**

- `doc/20-pages.md` §6 与 `doc/70-viewer.md` §4 只有**乐观返回**，没有前进方向的投机导航，也没有"边出现次数"这个门槛。

**优先级**：MEDIUM

---

## 20. 甩动落点预测式预取

**是什么**

- A 端本地跑惯性物理，算出这次甩动的落点并**提前**告诉 B。
- B 不必逐步跟随，而是直接跳到落点、优先预取落点区域，之后再回补甩动途中掠过的那段内容。
- 调度上按"视口何时会到达该块"与"送达需要多久"排序排序，来不及的块推入空闲回填队列。

**依据**

`old/llm-think.txt:1116-1120`（"since the viewer computes where the scroll will land, A can send that target ahead of time, letting B jump the capture head there and prefetch the landing area first, then backfill the content the fling passes over"）、`:1527-1528`

**与现有文档的关系**

- `doc/30-scroll.md` §4 的采集头调度只有"运动方向前方 + 提前量"与"静止时两侧预取"；§3 的"远距离跳转"仅指滚动条拖拽。

**优先级**：MEDIUM-HIGH

---

## 21. 字形级字典：投影切字格 + 背景归一化

**是什么**

比文档里"按连通域 hash 文字"更可实现的算法：

- 先对行投影找文本行，再在行内对列投影、按空白间隙切成字符格。
- 每格 bitmap 哈希进字典；一行文本变成一串 **cell ID + x 偏移**（**2–3 字节/字形**，对比 zstd 后的光栅 50–150 字节）。字典未命中则回退到该格位图按索引平面编码（CJK 48px 约 100–200 字节）。
- 关键：格子存的是**去掉背景、按行主色归一化后**的形状 → 同一个字形在白底、按下灰、卡片底上都能复用。可选按调色板亮度序做规范化以跨配色复用。
- 配套**多级 XY-cut 层次**（条带 → 条目 → 行 → 格），每级一个内容 hash，发送方取命中的最大层级。

**依据**

`old/llm-think.txt:201-220`（"segment text into character cells: project rows to find text lines, then project columns within each line to split at blank gaps… roughly 2–3 bytes per glyph versus 50–150 bytes… cells have to be stored without their background, normalized against the line's dominant color"；"a multi-level XY-cut hierarchy, where repeated strips, items, lines, and cells each get a content hash and the transmitter uses the largest level that hits"）

**与现有文档的关系**

- `doc/40-codec.md` §12 只把"字形级精灵（按连通域 hash 文字）"列为消融候选项 —— 那是另一种方法，也不是算法规格。
- `old/10-codec-modules.md` 记录过字形精灵的离线收益只有 −8.4%，且因 11.4/255 的误差被否 → 属于要先量后做的候选。

**优先级**：MEDIUM-HIGH（收益未实测，需先做 oracle 上界，见第 25 条）

---

## 22. 抗锯齿文字的 α 平面编码（前景/背景二色 + 4-bit alpha + escape）

**是什么**

灰度 AA 意味着三个通道都是同一个 alpha 的函数 → 编**一个 α 平面 + 小残差**能把原始符号数砍到约 1/3。具体：一个块编成（背景色, 前景色）+ **4-bit alpha 平面**，套不进这两色直线的像素用 escape 像素显式编码。原文明确归类为"要量、不做承诺"的一类。

**依据**

`old/llm-think.txt:187-190`（"an α-plane with a small residual would beat plain zstd on RGB, since gray AA text makes all three channels functions of one alpha value, so coding one plane could cut the raw symbols roughly threefold"）、`:452-457`

**与现有文档的关系**

- `doc/40-codec.md` §6 把所有文字都走 `Lit{Palette}`（调色板 + 索引平面 + zstd），没有 alpha 变体；§12 也没有。
- 与第 8 条（局部调色板 + escape）同族：AA 文字的色数爆炸正是 palette 需要 escape 的主因。

**优先级**：MEDIUM

---

## 23. 增量行 hash + 零拷贝读帧（加高屏下是 CPU 预算问题）

**是什么**

- 加高到 3× 屏高后，逐帧全量算行 hash 会吃满 CPU。
- 做法：ImageReader 的 RGBA buffer 在 JNI 侧**就地读**（零拷贝）；用 memcmp 把每个行带与上一帧比对，**只有变化的行**才重算 hash；行 hash 用 SIMD 一次算多条 64 位。
- 配套数字：3× 屏高一帧约 **34 MB** RGBA；hash 成本约 ×3 ≈ **6.6 ms/帧**；ImageReader 没有 damage 信息 → 每帧都要扫。

**依据**

`old/llm-think.txt:4326-4327`（"ImageReader's RGBA buffer can be read in place over JNI with no copy, and I'd compare each row band against the previous frame with memcmp so only changed rows get re-hashed"）、`:4310`、`:2289-2290`

**与现有文档的关系**

- `doc/60-host.md` §3 只写"回调里只取最新一帧，去掉行跨距后交给 rd"；`doc/40-codec.md` §2 只定义行 hash 的语义。
- 没有增量哈希与零拷贝的实现约束，也没有"加高后 hash 成本 ×k"这条预算说明。

**优先级**：MEDIUM

---

## 24. 缓存命名空间要补 App 版本

**是什么**

渲染环境 hash 应加上 **App 版本**（版本码）。App 更新后旧缓存仍会命中——hash 校验能挡住错误像素，但"乐观返回"式的推测绘制会闪出旧内容，且旧缓存只是浪费存储。

**依据**

`old/llm-think.txt:2544-2545`（"a render-environment hash to namespace the cache, built from width, density, font scale, night mode, locale, and app version… speculative paints from a stale environment could flash the wrong content"）

**与现有文档的关系**

- `doc/20-pages.md` §2 的几何键是"虚拟屏宽、dpi、字体缩放、深浅色" → 缺 App 版本。
- `doc/80-corpus-bench.md` §3 的 manifest 反而已经记了"包名与版本"，两头不一致。

**优先级**：MEDIUM（改动小，但属缓存正确性的硬缺口）

---

## 25. 识别原语用"注入的滚轮增量"当离线真值打分 + 实现前先算 oracle 上界

**是什么**

**真值打分**：因为注入的滚轮 delta 是已知的 → 每一帧滚动的**命令位移**可以给位移检测器离线打分；A11y 的 ImageView 边界可以给"图片矩形分类器"打标签；超高屏采集可用来核对拼接出的画布与 1× 采集逐像素一致。各识别原语按相对离线 oracle 的 **regret** 排序。这样"识别层只用基础信号"这条约束变成可检验的，而不是口号。

**oracle 上界**：在实现某个昂贵特性（如字形级精灵）之前，先离线算两个理论上界——`oracle dedupe`（地图 + IBC 的最优去重能省多少）与 `oracle static`（用最强无损压缩器做字面量的下限）。例：重复文本页上每个重复字形从约 100 字节（zstd 序列）降到约 4 字节（精灵引用），潜在约 2× 收益。上界不值得算清楚，就不要投入实现。

**配套**：启发式参数不能在同一批语料上既调又评 → 一个设备 / 一个 App 子集上调参，**留出 xiaomi 设备与部分轨迹**做 hold-out，每个场景录两个种子只用一个调；评分要对照"命令下发的滚动量、A11y scrollY、慢速拼出的长截屏"三种真值。

**依据**

`old/llm-think.txt:7287-7288`（"the injected wheel deltas are known, so each scroll frame's commanded displacement can score the displacement detector offline, and the ImageView bounds can label the image-rect classifier"）、`:7313`、`:2499-2509`、`:1821-1822`、`:1240-1241`

**与现有文档的关系**

- `doc/80-corpus-bench.md` §8 是"逐个关掉特性看字节 / 耗时变化"（事后消融），§4–§6 有场景与 bench —— **没有"用注入增量当真值给检测器打分"、没有 regret / oracle 排序、没有数据划分与防过拟合约定**。
- `doc/80-corpus-bench.md` §3 已有"1× 与加高各录一次核对画布"，但只是核对，不是打分机制。

**优先级**：MEDIUM-HIGH —— 这是所有识别原语的验收方法。

---

## 26. 有损层用浏览器原生解码器；fg 可放弃逐像素一致

**是什么**

fg 是**独立合成**的一层，B 从来不需要 A 的 fg 像素 → 有损图不必在 rd 里用 Rust JPEG 编解码：

- A 端直接用浏览器的 `createImageBitmap` / `ImageDecoder` 解码，更快也更简单。
- 由此 **"0px 不变式"缩小到 bg / lit 两层**；fg 层不再承担逐像素一致的要求。

代价与约束：

- B 端无法预知浏览器解码的结果 → 有损区必须**始终**排除在行 hash 与校验之外（B 端按源像素算 hash，A 端无从复现）。
- bench 的判定口径不变："有损区之外逐像素一致"。

这条与 `doc/40-codec.md` §7"编解码都在 rd 里（纯 Rust 的 JPEG 库即可），两端和 bench 用同一份代码"**方向相反**——但更快、更简单，且不影响 bg / lit 的保真承诺。

**依据**

`old/llm-think.txt:8203-8207`（"native browser decoders (createImageBitmap or ImageDecoder) could replace a shipped WASM JPEG decoder, since the foreground layer is composited separately and never needs to match A's pixels exactly… I'd revise the invariant so the 0px check covers only those layers"）、`:8215-8219`

**与现有文档的关系**

- `doc/40-codec.md` §7 要求两端同码；§10 的校验跳过项虽然已排除有损像素，但没有"fg 层可放弃确定性"的层级规则。
- `doc/70-viewer.md` §3 的合成顺序没有区分 fg 与 bg / lit 的保真要求。

**优先级**：HIGH —— 协议级决定（是否还要在 rd 里带一个 JPEG 库）。

---

## 27. fg 的合成协议：bg 条带开"洞块"，tile id 参与条带 hash

**是什么**

- 每条带同时列出"有损 tile 摆放"和"无损块"；bg 平面上的**洞块**让下面的 tile 透出来。
- 圆角图标的兜底：整个包围盒按 JPEG 编，**边界块用便宜的平色填充**，再由 bg 平面用精确的按块掩码把这些边界块盖回去。
- tile id = 源像素 hash；同 id 的更高质量版本直接替换低质量版本。
- **条带 hash 必须覆盖：无损平面 + 洞掩码 + tile id**。两端各自算完解码后互相校验。
- 代价（明确接受）：分类参数一改 hash 就变，调过参的页会缓存失效。

**依据**

`old/llm-think.txt:8209-8212`（"the hole blocks in the background strip let the tile show through, so each strip just lists its tile placements alongside the lossless blocks"；"the bounding rectangle gets JPEG-encoded as a whole, with the boundary blocks filled with a cheap flat colour, and the background plane then overdraws those boundary blocks"）、`:8399-8403`

**与现有文档的关系**

- `doc/40-codec.md` §7 只写"图片矩形里与底色相同的像素用 1 位掩码标出"；§2 的行 hash 只覆盖行像素 → 没有"洞块 + tile id 参与条带 hash"的规则。

**优先级**：MEDIUM

---

## 28. 下行发送分级 + 8–16 KB 消息粒度 + 服务端 QUIC 流优先级

**是什么**

- 优先级顺序：**控制 → 精确可见补丁 → 有损图片与精修 → 预取与投机工作**（最后）。
- 每条消息控制在 **8–16 KB**，让可见补丁不会长时间排在已发出的低优先级负载后面。
- 优先级要设在 **B 自己的 QUIC 发送流**上（quinn / wtransport 的 `SendStream::set_priority`）——浏览器的 `sendOrder` 只管浏览器自己发起的流。

**依据**

`old/llm-think.txt:261-265`（"control first, then exact visible patches, then lossy image and refinement, with prefetch and speculative work last. I'd keep each message to about 8–16 KB so a visible patch never waits long behind something low-priority already in flight"）、`:489-493`

**与现有文档的关系**

- `doc/50-protocol.md` §5 已写"B 给 `ctl` 的发送优先级高于 `tx`"以及 `sendOrder` 的适用范围；`doc/10-dataflow.md` §6 已写"可见区永远优先，一次只发一小段" → 原则已有，**缺四级阶梯与 8–16 KB 这个可实现的粒度**。

**优先级**：MEDIUM

---

## 29. 超高虚拟屏的成本模型 + 改高度的方式 + 逐页前置判定 + 1× 先行

**是什么**

**成本模型（为什么不是免费的）**

- 1080×9600 的 RGBA 帧约 **41 MB**，四个缓冲约 **166 MB**。
- 每帧多哈希 9600 行约 **+9 ms**。
- 高度受 GPU 最大纹理尺寸封顶（3× = 1080×7200 通常可以，4× 可能超过 8192 上限）。
- 每次 resize 都是一次配置变更 → App 会重建界面、丢滚动位置。

**改高度的方式（已拍板的方向）**

**优先在活动之间修改屏幕高度，每个活动的高度保持不变。** 也就是：高度不是"每页 resize"，而是**在活动切换时**调整一次，进入某个活动后该活动全程用固定高度；单个活动内部不反复 resize。

- 好处：避免同一活动内的重复配置变更与界面重建；同一活动的页面可以共享同一套几何键与缓存。
- 代价：活动内部如果要换高度仍需 resize（这条留给"活动中途确实需要"的情形）。
- 另一条相关但更弱的替代路线：**永远保持一块固定高度的高屏**，只改屏内任务 / 窗口边界——同样会重建未声明 `configChanges` 的 Activity。

**逐页前置判定**

逐页加高前先查目标 Activity 是否声明了 `screenSize` 等 `configChanges`（PackageManager 可报）→ 声明了 resize 便宜（不会重建），没声明就退回"整会话固定高度"。

**验证方法**

**先做 1× 采集头扫描作为基线**（它没有任何平台风险），再用 bench 的页面模拟器比较加高倍数 1 / 2 / 3 的**白块率**来定 `tall_k`，而不是靠直觉。

**依据**

`old/llm-think.txt:7007-7008`（"a 1080×9600 RGBA frame is about 41MB, so four buffers come to roughly 166MB, and hashing 9600 rows per frame would add around 9ms"）、`:7345-7350`、`:3224-3238`（"check whether the app's activity already declares those config changes as handled… If it does, resizing the display for a page is cheap, since nothing gets relaunched"）、`:2934-2935`（"I'd run a page simulator comparing tall display factors of 1, 2, and 3 on white-block rate"）、`:2289-2290`

**与现有文档的关系**

- `doc/30-scroll.md` §5 只有 Growing / Tall / Shrinking 状态机与 resize 的副作用描述，没有内存 / 哈希成本估算、没有"活动之间改高度、单个活动高度不变"这条规则。
- `doc/60-host.md` §9 的 P3 只问"最高能加到几倍"，没有"事先查 configChanges 声明"。
- `doc/90-tasks.md` M4 的顺序是先做加高屏状态机（42）再做 bench 页面模拟（43）→ 与"1× 先行 + k 值对比"的次序相反。

**优先级**：HIGH

---

## 30. 页面身份：像素为主，骨架降级为"分区 / 确认"

**是什么**

A11y 树晚到（每节点 50–300 ms 的 IPC），像素必须先跑：

- 算新帧的行条带 hash，查**持久化的 per-App 行段索引**；多数条带命中同一已知页即认定该页，并从其缓存复制。
- 骨架 hash 只用来**分区、确认**，以及预测"点了这个条目会去哪"。
- 因为设置子页共用骨架，缓存键改为"包名 + Activity + 标题"下挂**多个布局变体**，变体之间用**像素行 hash 重叠度**挑选 → 碰撞只浪费字节，不影响正确性。
- 落地形式（两级查找）：**骨架把候选页分桶 → 桶内用首帧行哈希投票选出最佳匹配**，即使滚动位置不同也能认出同一页。

**依据**

`old/llm-think.txt:173-176`（"hash row bands of the new frame and look them up in a persisted per-app index… the accessibility skeleton hash kept for partitioning, confirmation"）、`:508-513`（"keep a few layout variants under each key, and pick the best match by pixel row-hash overlap. Collisions would then cost extra bytes, not correctness"）、`:4069-4073`、`:4654`

**与现有文档的关系**

- `doc/20-pages.md` §2 的 `PageId = hash(包名, Activity 类名, 窗口标题或顶栏标题文本, 骨架, 几何键)`，且"PageId 只用来找缓存候选，是不是真的同一页由像素决定" → 原则一致，但**没有全局行哈希索引 + 桶内投票**这个两级查找算法。
- `doc/20-pages.md` §9 的补救是"发现后把能区分它们的文本加进 PageId"，而实测（见本条"依据"里的骨架碰撞与标题缺失）标题常常在滚动区里取不到 → 现有补救在设置类页面上不可用。

**优先级**：HIGH

---

## 31. 折叠标题栏 → 区域矩形是逐帧数据，不是页面常量

**是什么**

语料实测：xiaomi 设置页的 RecyclerView 顶部在带搜索栏时是 **453 px**、标题收起后是 **147 px**；activitylauncher 的区域 top 在同一页内取 **108 / 150 / 296 / 464** 四个值 → `Region.rect` 必须**可逐帧更新**，不能当页面常量。

推论：

- 加高屏的"顶栏 + 滚动窗口 + 底栏"合成要**跟随列表锚点**，而不是固定的屏幕行。
- 折叠带由 B **流式下发**（权威），列表带本地合成。
- 这也影响页面身份：骨架 hash 里含区域矩形，矩形变了就等于换了页。

**依据**

`old/llm-think.txt:5798-5803`（"its top edge shifts with scroll offset, so the scroll region's rectangle isn't fixed"）、`:5863-5867`（"two different region-top values for the Xiaomi settings main page, 453 and 147"）、`:6024-6033`（"the collapsing band gets streamed from the phone as authoritative, while the list band is composed locally"）

**与现有文档的关系**

- `doc/30-scroll.md` §8 把折叠标题栏列为边界情况（"滚动区 rect 在变，或同一帧有两个位移 → 收起过程中的帧不写画布"）；`doc/20-pages.md` §3 允许 B 发 `Regions` 修正 → 但没有"区域矩形是逐帧数据"这条规则，也没有语料证据。

**优先级**：MEDIUM-HIGH

---

## 32. 悬浮控件的矩形要从行 hash 里挖掉（不只是不写画布）

**是什么**

FAB 压在滚动内容上时，它那块的像素会混进被压住那一行的**行签名**，使该行的 hash 随 FAB 状态变化而失配 —— 哪怕那一行其实没变。

做法与右缘滚动条带一样：把这些矩形从**行签名的计算范围**里裁掉（只从"该控件不在此行"的帧取像素），而不只是绘制时避开 / 那一帧不写。

**依据**

`old/llm-think.txt:9299-9300`（"the FAB overlapping scrolling content needs its rect carved out of the row signatures, the same way the scrollbar band is handled"）、`:9312-9313`

**与现有文档的关系**

- `doc/40-codec.md` §2 的行 hash 只裁掉右缘滚动条窄带 `sb_w`，其余按区域宽度。
- `doc/30-scroll.md` §8 的 FAB 处理是"切成压在上面的 `Fixed` 区域。被它挡住的画布行这一帧不写" → 解决了"不写"，没解决"不参与 hash"。

**优先级**：MEDIUM（与第 2 条的列带有效位配套后更有价值）

---

## 33. 滚动条窄带靠"重叠采集"重建，不必等淡出

**是什么**

现在文档的做法是"等滑块淡出后用那一帧补写窄带"。另一条：**让相邻两次采集的重叠量 ≥ 滑块长度**，于是每一条内容行至少在有一次采集里没被滑块压住，可从那一次取到干净像素 —— 不必等淡出。

配套事实：滑块的淡出由 ViewConfiguration 驱动（约 **300 ms 延迟 + 250 ms 淡出**），**不随动画缩放缩短** → 关掉动画并不能让它提前消失，所以"等淡出"这条路的延迟是固定的。

**依据**

`old/llm-think.txt:3248-3252`（"rebuild those edge pixels from frames where the thumb sits elsewhere, since each content row should appear at more than one thumb position if the overlap is large enough"）、`:3518`

**与现有文档的关系**

- `doc/30-scroll.md` §7 只有"采集头停稳、滑块淡出之后，用那一帧补写这条窄带" → 没有重叠采集这条路，也没有淡出时序的事实。

**优先级**：MEDIUM

---

## 34. 证书轮换免重配对

**是什么**

自签证书有效期 < 14 天，到期换证后老 URL 会失效。做法：

- 每次连接时 B 在控制通道里**预告下一个证书的 hash**，A 把当前和下一个 hash 一起交给 `serverCertificateHashes` → 轮换不需要用户重新配对。
- 更彻底的路线：A 每次连接前从 B 的 tailnet 地址用明文 HTTP 取当前证书 hash（需验 CORS 与 Private Network Access 是否放行）→ 彻底不需要轮换协议。注意该 hash 不是秘密（任何能连到端口的设备都能从握手里算出来），鉴权仍靠独立 token。

**为什么这条重要（已确认的方向）**

**确认可以从远端共享查看屏幕** —— 这个方案本身就是"从远端（另一台机器 / 另一台手机 / 浏览器）看 B 的屏幕"的能力来源；证书与配对是这条链路上唯一的"必须提前做对、否则远端接入要重新配"的环节。以后版本也可以视为**App 运行的线上分享查看器**：App 在 B 上跑起来，A 在任何安全上下文里打开就能看。

**依据**

`old/llm-think.txt:3437-3449`（"on each connection B would announce the next certificate's hash in the control channel, and A would pass both the current and next hashes to serverCertificateHashes"）、`:7158-7168`、`:7563-7565`（"A can fetch the current hash over tailnet plain HTTP before connecting, so no rotation protocol is needed. I still need to verify CORS and private-network access for that fetch"）

**与现有文档的关系**

- `doc/50-protocol.md` §5 写的是"证书落盘复用，快过期时换新并打印新 URL" → 现状是**要求用户换 URL**，没有免重配对机制。

**优先级**：MEDIUM

---

## 35. 手机当 A：PWA 从任意 HTTPS 源连接【有意义，标出来】

**是什么**

WebTransport + `serverCertificateHashes` 在**任何安全上下文**可用 → A 是手机时，不必在手机上自建 localhost 静态服务器：由任意 HTTPS 源托管一个 PWA 直连 B 即可（剪贴板在 HTTPS 源下同样属于安全上下文）。

**为什么这条有意义（值得单独标出）**

- 它把项目形态从"本机调试工具"扩到"**可以在任何设备上打开一个网页就看 B 的屏幕**"：远端分享查看器不需要用户先在手机上装东西、也不需要手机本机起服务。
- 它同时回答了 `GOAL.md` §7 第 6 条（"手机当 A 时 localhost 页面由谁提供"）标为"未定"的问题。
- 与第 34 条（证书轮换免重配对）合在一起，构成"远端接入"这条链路上最后两块拼图：入口在哪（PWA）、怎么免重配（预告下一个 hash）。
- 注意前提：PWA 源必须是 HTTPS；明文 HTTP 的 tailnet 地址不是安全上下文，页面拿不到 WebTransport 与剪贴板。

**依据**

`old/llm-think.txt:3595-3596`（"A possible route is a PWA from any HTTPS origin connecting to B over WebTransport with serverCertificateHashes, which sidesteps the localhost requirement"）

**与现有文档的关系**

- `GOAL.md` §7 第 6 条："手机当 A 时 localhost 页面由谁提供｜**未定**，先只保证桌面浏览器"。
- `doc/70-viewer.md` §1 写"必须是 `http://localhost`，否则 WebTransport 和剪贴板不可用" → 缺"任意 HTTPS 源也满足安全上下文"这一条。

**优先级**：MEDIUM —— 但它是"线上分享查看器"这个产品形态的入口条件。

---

## 36. 浮窗四分类 + 加高模式下的重锚

**是什么**

把浮层窗口按位置分成四类，各有各的处理：

| 类别 | 处理 |
|---|---|
| 与顶栏重叠 | 保持固定（随顶栏走） |
| 贴底 | 重锚到视口底 |
| 居中 | 在视口内重新居中 |
| 其余 | 随内容偏移移动 |

弹窗在**超高虚拟屏**上居中会跑到超高屏中部（而不是用户看到的 1× 视口中部）→ 要在 A 的视口内重新居中，并把点击按同一偏移**反算**回 B 的坐标。

**依据**

`old/llm-think.txt:4096-4101`（"since the dialog is its own window with known bounds, I could recenter it over the page's viewport and map taps back through the same offset… I'm classifying each floating window by where it sits: top-chrome-overlapping ones stay fixed, bottom-flush ones re-anchor to the viewport bottom, centered ones recenter, and the rest move with the content offset"）

**与现有文档的关系**

- `doc/30-scroll.md` §5 的加高模式只拆顶栏 / 底栏两段；§8 的边界情况写"出现模态窗口 → 缩回"；`doc/20-pages.md` §6 的模态页只有 `rect` + `dim`。
- 没有浮窗分类表，也没有"弹窗在加高模式下重新居中 + 点击反算"这条规则。

**优先级**：MEDIUM

---

## 37. 两段式图片投递 + 占位分辨率

**是什么**

有损图片不一次到位：

- 先发 1/4 分辨率占位，**仅当**估算传输时间超过约 **100 ms 预算**时才发占位。
- 再升级到全尺寸。
- 滚动跟不上链路时同理：先发低分辨率条带，滚慢后再换成精确内容。

相当于把 tile 地图的"粗 tile → 细 tile"策略用到图片与滚动上。

**依据**

`old/llm-think.txt:1744-1745`（"a two-pass approach, sending a quarter-resolution placeholder first and refining to full size, but only when the estimated transfer would exceed about 100 ms of budget"）、`:1840-1841`（"I'd send a low-resolution strip first as a placeholder and swap in exact content once the scroll slows, the same way tile maps load coarse tiles before sharp ones"）

**与现有文档的关系**

- `doc/40-codec.md` §7 只讲整图有损编码、质量旋钮 `img_q` 与截断图处理 → 没有渐进 / 占位档。
- `doc/30-scroll.md` §2 的占位只是"区域底色"的纯色块。

**优先级**：MEDIUM（与第 10 条的两档阶梯同族，可合并实现）

---

## 38. 旧字节口径必须重算：`back` 的 48.6% 是桌面壁纸

**是什么**

旧基线里 `back` 类占下行 **48.6%**，看着很吓人——但这些帧多数是 App 退到桌面后的**桌面壁纸**，单 App 会话的新设计根本不会产生。旧 `back` 帧里 21/87 超过 2 万色，最大的一帧就是壁纸。

因此在做容量 / 预算之前，必须先按"栈顶包名 != 会话 App"打 `in_scope=false` 剔除，再重算字节分布。语料侧要落地的是：**每帧记录实际的栈顶包名**（现在 `app` 字段记的是"打算采的 App"）。

**依据**

`old/llm-think.txt:4050-4051`（"the 'back' frames that look so heavy may be the app exiting to the launcher and showing the wallpaper, which the new design wouldn't send at all… I'd want to recompute it before trusting it"）、`:4584-4586`

**与现有文档的关系**

- `doc/80-corpus-bench.md` §2 已有"越界帧打标并从统计里剔除"和"每帧记实际的栈顶包名"两条规则 → **规则有，但缺这一量化结论**（48.6% 不可直接用于预算）。

**优先级**：MEDIUM

---

## 39. 用法加权的 bench 主指标：每分钟字节数 + 按交互类别拆解

**是什么**

不要只看场景总字节。主指标应是**一段脚本化真实会话的每分钟字节数**，并按交互类别拆开：**首访 / 回访 / 新滚 / 回滚 / 点击**。退到桌面的帧标为越界，不进统计。

这是对项目真实约束（300–800 KB/s 链路）的直接回答："一个人正常用这个 App，每分钟花多少 KB"。

**依据**

`old/llm-think.txt:335-340`（"a usage-weighted score, like bytes per minute over a scripted realistic session, with results broken out by interaction class such as first visit, revisit, new scroll, scroll back, and tap"；"launcher frames from backing out of the root and recovery frames get flagged as out of scope"）

**与现有文档的关系**

- `doc/80-corpus-bench.md` §6 列了按场景 / 按层 / 按 Op / 按步骤分的字节、白块指标、影子编码省流 → **没有用法加权的速率指标，也没有交互类别分类法**。

**优先级**：MEDIUM

---

## 41. A11y dump 绑定采集帧序号 + 分割结果的有效范围

**是什么**

- 每次取到的节点树要打上它对应的**采集帧序号**，才知道它到底匹配哪些帧。
- 由它推出的区域分割**只对"其后没有内容变化"的那些帧有效**（或分割边界与真实像素不连续处对齐时才有效）→ 否则 A11y 的区域会把错误的内容框住。
- 取树本身在 binder IPC 上要走几十毫秒 → 只在 content-changed / scrolled / window 事件去抖后重取。

**依据**

`old/llm-think.txt:3404-3409`（"Each dump gets tagged with the capture frame sequence number so I can tell which frames it actually matches, and its segmentation only applies to frames with no content change in between"；"walking the nodes over binder IPC can take tens of milliseconds"）

**与现有文档的关系**

- `doc/60-host.md` §5 已写"每棵树带时间戳和它对应的窗口"；`doc/10-dataflow.md` §1 第 2 步的候选"都带时间戳" → 已有的是**时间戳**，没有**帧序号绑定**与**分割有效范围**这条规则。

**优先级**：MEDIUM

---

## 42. 看门狗进程还原系统设置

**是什么**

现有三处还原（正常退出、崩溃处理器、下次启动兜底）之间有一个空窗：host 被厂商后台管理杀掉、又没触发崩溃处理器时，设置会一直保持被改的状态直到下次启动。

补一个小的 **watchdog 进程**：一个 shell 循环，监视 host 的 PID，PID 消失就立刻还原动画与模糊缩放（读 `/data/local/tmp/hj/restore.json`）。

**依据**

`old/llm-think.txt:5194`

**与现有文档的关系**

- `doc/60-host.md` §2 依赖退出 / 崩溃处理器 / 下次启动三处；§8 已列"host 无声消失（厂商后台管理杀进程）"这个症状 → 但没有对应的设置还原手段。

**优先级**：MEDIUM-LOW

---

## 43. 区域"同 bounds 合并"规则

**是什么**

语料里多区域页只有两种形态：

- (a) **ScrollView 套 ScrollView** —— razr 设置页；设置主页更是 ScrollView 套 GridView，子页是顶栏下单个 ScrollView。
- (b) **ScrollView 套 ViewPager 套列表** —— libchecker。此时 ViewPager 与 RecyclerView **与外层 ScrollView 同 bounds，应合并成一个区域**；"多个区域"其实来自外层那条全屏 ScrollView。

这给 `doc/20-pages.md` §3"只取面积最大的那条纵向滚动链做 `Scroll`"补了一条可执行的补充规则：**bounds 相同即合并**。

**依据**

`old/llm-think.txt:8809-8815`、`:9155`（"the main page nests ScrollViews around a GridView, and the subpages each have a single ScrollView below the top bar"）、`:9226-9230`（"a ScrollView wrapping a ViewPager and RecyclerView share the same bounds, so they collapse into one region"）

**与现有文档的关系**

- `doc/20-pages.md` §3 只有"只取面积最大的那条纵向滚动链"；`doc/30-scroll.md` §9 把横向容器推到 M6 → 没有"同 bounds 合并"与实测形态清单。

**优先级**：MEDIUM

---

## 44. 归一化互相关作为位移回退 + 稀疏锚点定位

**是什么**

**(a) NCC 回退**：预测位移失败时回退到行签名的**归一化互相关**投票 —— 比纯 hash 匹配更能容忍轻微拉伸、重采样、亚像素位移。

**(b) 稀疏锚点**：不给每一行建索引，只保留"行 hash 命中固定余数"的内容定义**锚点**，索引体积缩约 **32×**，且用 winnowing 保证锚点的最小密度 → 回到一个页面时，用首屏的一小段 hash 就能在页面地图里定位，**与是否对齐无关**。页面查找同理：给每小段行一个 anchor hash，让新帧按内容"投票"选页面。

**依据**

`old/llm-think.txt:2069-2073`（"the row-signature and normalized cross-correlation fallback"）、`:2325-2327`、`:6228-6232`（"instead of indexing every row, I'd keep only content-defined anchors (rows whose hash hits a fixed residue), so the index shrinks by about 32x… Winnowing fits here because it guarantees a minimum density of anchors"）

**与现有文档的关系**

- `doc/40-codec.md` §3 是"整表 + 投票"，没有互相关回退、没有稀疏锚点索引 / winnowing。
- 与第 30 条（per-App 行段索引）配套：第 30 条要的是"查全局索引"，本条给的是"索引怎么建才不爆"。

**优先级**：MEDIUM

---

## 45. 有损图片路径应前移到早期里程碑

**是什么**

照片类矩形占语料字节的大头：**面积约 52%、字节约 73%**。如果图片编码排在最后，所有其它优化的测量都会被照片字节淹没 → 有损静图编码应**提前**（先做"图片"，再调滚动 / 引用）。

次序原则：**先建内容寻址 tile store，再叠滚动识别与预取**。

这与 `GOAL.md` §6 / `doc/90-tasks.md` M5 把图片排在最后**相反** —— 是规划性变更，需要显式决定而不是默默改。

**依据**

`old/llm-think.txt:6699-6701`（"photo-heavy images dominate the bytes… lossy image coding looks like it should move up from the late compression stage to an early milestone… photos… would swamp any measurement of the other optimizations until they're handled"）、`:753-757`（"images cover about 52% of the area but account for 73% of the bytes"）

**与现有文档的关系**

- `GOAL.md` §6 把有损图片放在 M5（压缩）；`doc/90-tasks.md` M5 任务 50 才是"图片：判定、有损编码与掩码"。

**优先级**：MEDIUM —— 影响里程碑排序与首屏字节目标的可达性。
