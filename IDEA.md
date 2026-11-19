# 算法开发计划

活镜投屏（A→B 白名单开/点/滑/返, B→A 单画布/调试长截屏/记返回栈IBC）

预期环境：专为Tailscale等 300~800KB/s 网络，低码率高还原优化，单兼容 WebTransport QUIC。

只能用 11 个白名单 App： `com.android.settings,com.absinthe.libchecker,com.sirma.mobile.bible.android,de.szalkowski.activitylauncher,com.azefsw.audioconnect,com.android.calendar,com.miui.calculator,InfinityLoop1309.NewPipeEnhanced,org.qpython.qpy,com.and96.aggregator_news,com.aefyr.sai`。 新增两个的用途：InfinityLoop1309.NewPipeEnhanced（切 tabs）、org.qpython.qpy（Notebook / 编辑器 / 文件 UI）

- 布局层bg / 精灵池lit / 视频区fg（三层）独立质量UI可调，r键可切换查看：最近刷新的矩区/图文视三类区遮罩图，方便用户验证划分
- **adb shell 权限虚拟屏 → 浏览器/手机端 WASM，Kotlin+Rust{enum,wire} 端到端（图片图层有损 其他层0px还原）**
- 【重要】B端默认要做滚动区prefetch。不能逐像素条的同步，也要小心处理滚动条和分app预取距离（以延迟计）。 方法是超高虚拟屏，对A11y测出100vw底栏，或滚动区贴到屏底的【单个页面】默认开
- 分App-分页面（A11y 树剔除列表骨架resource-id 结构hash）的持久缓存，往返不丢。典型示例是设置App，用久了就像许多长截屏构成树一样，协议带 cache_rev pg_rev，B只发差异

【流压缩思路】

1. 像 tile 地图/长截屏一样理解数据面，行哈希投票找出位移。新地块 = 字面量，滚回来直接贴。返回上一页时整屏贴回，:active 高亮等短变更不改底图缓存
2. **Palette Mode（文字和symbolic图标锐利 vs 图片有损） + IBC（内容hash单行/所有行hash折叠，重复只发COPY）**。纯色行会产生大量伪匹配，应按行的信息量加权并限制在A11y滚动区内，并裁掉右缘滚动条列
3. 可逆变换（jpg/zstd/基础残差编码），只对"像图片"的矩形用。可以评估字形级精灵（按连通域哈希）
4. 剩余脏矩形zstd。可以用"预测滤波 + zstd 长窗口，或以上一帧作字典"

借鉴 HEVC-SCC / AV1 Screen Content Tools（IBC + Palette Mode）+ 远程桌面哈希运动估计，针对屏幕内容（锐利边缘、重复模式、有限色数）优化。

对App内容模式启发式自适应，尤其是A11y的检测结果（可在l键展开的左上角图层-比重面板中看到相关可调键值）。 

默认界面为静态，只来回滚动/动返回栈不导致内容更新，除非单独设置。


【全局设置优化】

kt启动时关window/transition动画、关背景模糊，animator_duration可调。退出或crashHandler还原

没有状态栏和通知，没有HOME键。ESC或浏览器 history API 模拟back层级。以单应用为会话。host可以熄屏

**WebUI除l,r键，包含左键、滚轮、右键Esc返回，右上角选（动态白名单）包名，以及按键和 CtrlC/CtrlV 支持**

面板会显示精灵缓存（如返回键和反复开关单按钮）、滚动、长截屏所省流的百分比

Tailscale MTU 为 1280，注意 datagram 尺寸。WebUI要在localhost，证书用 serverCertificateHashes，放URL直接视为token鉴权即可。持久层用 OPFS + LRU

shell 身份可以用 UiAutomation 拿节点树，`FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`

【客户端滚动思路】

通过kt的A11y接口，你可以得到准确的滚动区大小、刚刚点击的位置、含图标的列表里图标的边界对齐（Card视图、纯布局图片【size基本会是一样的,x轴很容易对齐】 ）

以及弄清楚有没有tab区（需要横向滚动, HorizontalScrollView,DrawerLayout,..） ，主要为滑动动画优化

为了tile地图的质量，应该废除已知滚动区的【甩动手势】，确保涉及scroll{X,Y}的动画只由viewer合成。 用户使用右边缘滚动条时，也通过A11y数据，转换为tile合成 (scrollableOffset)，确保滚动条不会引起缓存失效。 在item高度无差异时，地图应是精确的，否则也能退化IBC

坑：Android 12+ 拉伸式越界回弹不是刚性平移，横向偏移布局协议也要表达。重进页面时滚动位置可能不同。圆角图标边缘混合了图片和背景。 DIM弹窗时backdrop压暗、整列平移、spinner等块运动补漏

越界拉伸不必进协议。视频层 fg 和横向 tab 建议放到最后

位移检测结果与 A11y scrollDeltaY 累计值交叉校验。两者不一致时以像素为准，并标记该页"地图不精确"。

prefetch只在空闲时发，A11y已到底时停。

提前量(px) = 滚速(px/s) ×（补块可绘制耗时 P95 + 安全余量）。耗时从 B 能调度该块算到 A 能画出，包含排队、传输和解码，不只看 RTT。可见区补丁永远优先。

【Material Design/M3/国行风格设计语言优化】

录制原始帧 + A11y dump + 输入序列，离线跑编码器比较字节数。没有它，启发式参数无法迭代，建议最先做。

bg 层优先调色板（主色 + 表面色变体），减少残差。

固定元素（状态栏、底部导航、FAB）

密度适配：支持多种 tile 大小与 DPI，国行高密度列表优先行级哈希。

【工程便利】

./old/ 下有一些老项目经验。 **我们计划放弃旧baseline，从确定性的交互轨迹数据**（Appium `node corpus/appi.js` 的精简重写/一键录制）和自动的全程体积/耗时/峰值脚本，与scrcpy算法对比，开始优化。

消融：同一份语料下逐个关掉编码器特性，看每个特性对「下行字节」和「编码耗时」的实际贡献；

设置SDK(mise -g)，以及启动每个工具的oneliner示例需加在 ./README
