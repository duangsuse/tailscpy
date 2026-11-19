# 活镜投屏 · 算法开发计划

## 1. 架构

| 模块 | 语言 | 职责 |
|---|---|---|
| host-adb | Kotlin (app_process) | 虚拟屏和 ImageReader、UiAutomation(A11y)、输入注入、全局设置开关与还原、剪贴板 |
| rd | Rust（JNI + WASM 同一 crate） | wire enum、行哈希/位移投票、分类、Palette/IBC/精灵/残差编解码、页面地图状态机 |
| host-net | Rust | WebTransport 服务端 |
| viewer | TS + WASM | 三层合成、本地滚动、持久缓存（OPFS）、调试 UI |

编解码两侧用同一份 Rust，保证 bg/lit 层逐像素一致。每条补丁带校验 hash，调试模式下 A 端做断言。

传输分流：
- 控制/输入、可见区补丁、prefetch 补丁走三条独立可靠流，用 `sendOrder` 保证可见区优先。
- prefetch 写入前检查可见队列为空，可被抢占。

## 2. 协议草案（Rust enum）

```rust
enum Up {            // A→B
  Hello{cache_rev, pages: Vec<(PageId, PgRev)>},
  Open{pkg}, Tap{page_xy}, ScrollTo{region, x, y, v_px_s}, Back, Key, Clip,
  Ack{seq, drawable_ts},          // 用于 P95 "可绘制耗时"统计
  Tune{key, val},
}
enum Down {          // B→A
  Page{id, rev, skeleton_meta, regions: Vec<ScrollRegion>, stack_op: Push|Pop|Replace},
  Region{id, rect, content_wh, offset_xy, at_top, at_bottom, bottom_bar: Option<Rect>},
  Tile{region, content_y, h, layer: Bg|Lit|Fg, body: Enc},   // 写入地图
  Overlay{rect, ttl_ms, body: Enc},      // :active/滚动条/spinner，不落底图
  Copy{src: (page, region, y) | SpriteId | PrevFrame(dx,dy), dst_rect},
  Dim{rect, color, alpha},  Shift{rect, dx, dy},             // 块运动补漏
  Sprite{id_hash, body},    Invalidate{page, range},
}
enum Enc { Solid, Palette{..}, Residual{filter, zstd}, Lossy{codec,q}, Raw }
```

## 3. 核心算法

**3.1 位移检测（行哈希投票）**
- 只在 A11y 滚动区内算，并裁掉右缘滚动条列。
- 每行算 64 位 hash，权重取该行颜色游程数，纯色行权重为 0。
- 对 prev 和 cur 的匹配行做 dy 直方图。峰值需过阈值且唯一，再做逐行验证。
- 结果与 A11y `scrollDeltaY` 累计值交叉校验。两者不一致时以像素为准，并标记该页"地图不精确"。
- 投票失败时退化为折叠行 hash（每 8/16 行）查全局字典的 IBC。

**3.2 页面地图**
- 页面 = 骨架 hash（包名 + 窗口 + resource-id 树结构，剔除列表 item 子树）。
- 每个滚动区是一条按内容坐标寻址的长条，带分段行 hash。
- B 只持久化 hash，A 持久化像素。握手时对齐 `cache_rev` 和 pg_rev，不一致就整页重置。
- item 高度变化或中间插入导致错位时，`Invalidate` 该段之后的内容并退回 IBC。
- 重进页面时滚动位置不同：用首屏行 hash 在地图里重定位。

**3.3 短变更不污染底图**
- 触摸按下后 N ms 内的变化，以及自动恢复为原 hash 的变化，走 `Overlay`。
- 变化稳定超过阈值才提交为 `Tile`。
- 滚动条同样走 Overlay 或直接丢弃，A 端按 Region 信息自绘。

**3.4 三层分类**
- 按块统计色数、梯度，并用 A11y ImageView 边界做先验：
  - 少色块 → bg，Palette 无损。
  - 小连通域且重复 → lit，精灵池，按连通域 hash。先做图标/按钮级，字形级在 M5 评估收益后再定。
  - 像图片的块 → fg，有损，质量可调，矩形对齐到 A11y 边界。
- 剩余脏矩形用预测滤波（Paeth 类）+ zstd 长窗口，上一帧同位置作字典。

**3.5 客户端滚动**
- 废除 fling。A 收到滚轮/拖动/滚动条拖拽后，本地立即合成，并发 `ScrollTo`。
- B 用末速度为零的拖动注入来移动"采集头"，补偿 touch slop。
- B 的真实滚动位置与 A 的视口解耦。
- 点击时把页面坐标换算回当前屏坐标，必要时先滚到位。

**3.6 超高虚拟屏和 prefetch**
- 启用条件：单滚动区页面，且有 100vw 底栏或滚动区贴屏底。
- 虚拟屏加高后，A 合成 = 顶栏 + 滚动窗口 + 底栏贴回。
- 超出加高范围时，空闲期由采集头前移。
- 提前量 = 滚速 × (P95 可绘制耗时 + 余量)，耗时由 `Ack.drawable_ts` 按 App 统计。
- `at_bottom` 时停止 prefetch。

**3.7 块运动补漏**
- DIM：检测 cur ≈ prev×k + c，发 `Dim`。
- 整列平移：发 `Shift`。
- spinner：周期帧进精灵池，循环用 `Copy`。

## 4. 里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| M0 可行性 | shell 下虚拟屏 + A11y；虚拟屏 resize 代价；WebTransport 打通；**录制语料 + 离线回放码率评测台** | 后续每个算法都能离线出"省流%" |
| M1 基线 | 脏矩形 + Palette/zstd、输入、白名单、动画/模糊开关与还原、r 键遮罩 | 静态页 0 字节，逐像素一致 |
| M2 滚动 | 行哈希投票、地图、viewer 合成、废 fling、滚动条转换 | 设置 App 回滚 0 像素字节 |
| M3 页面缓存 | 骨架 hash、pg_rev 差异、返回栈整屏贴回、Overlay 机制、OPFS 持久化 | 返回上一页本地出图，重连后不重传 |
| M4 prefetch | 超高屏、底栏合成、提前量控制、流优先级 | 匀速滚动无白块率达标 |
| M5 压缩深化 | 精灵池、全局 IBC、图片层有损、Dim/Shift/spinner、l 面板省流统计 | 对比 M1 的码率下降 |
| M6 收尾 | 横向 tab/Drawer、fg 视频层（MediaCodec → WebCodecs） | — |

面板的省流百分比用"影子编码"算：同一区域按字面量编码的字节数对比实际发送字节数，按精灵/滚动/长截屏分项。

## 5. 主要风险（M0 必须验证）

1. **安全上下文**：WebTransport 和剪贴板 API 都要求 HTTPS 或 localhost，直接开 `http://100.x.x.x` 不可用。可选方案是页面托管在 HTTPS 静态站或 A 端 localhost，再用 `serverCertificateHashes` 连自签证书（ECDSA，有效期 ≤14 天，需自动轮换）。
2. **虚拟屏 resize**：会触发配置变更，部分 App 会重建 Activity 或丢滚动位置。备选是"进入 App 时定高，不逐页切换"。
3. **超高屏副作用**：显存和纹理高度有上限，建议 ≤3~4 倍屏高。App 会多加载数据和曝光埋点。居中弹窗位置会偏，所以要限定单滚动区页面。
4. **UiAutomation**：虚拟屏上的窗口需要 API 30+ 的多 display 接口。默认会压制其他无障碍服务，需设不压制 flag。
5. **RecyclerView 无绝对偏移**：只有 delta，累计会漂移，所以地图坐标必须以像素投票为准。
6. **设置还原**：原值先落盘到 `/data/local/tmp`，kill -9 后下次启动可补还原。
7. **Tailscale MTU 1280 和 DERP 中继**：P95 抖动大，余量参数要自适应。
