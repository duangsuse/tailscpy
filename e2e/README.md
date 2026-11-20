# e2e · 人工录制、回放、场景

## 人工录制的 jsonl（`e2e/cases/<包名>.jsonl`，每个 App 一份）

`node e2e/recorder.mjs` 由**本地服务直接落盘**到 `e2e/cases/`（`--cases <目录>` 可改）：启动时先验证目录能建、能写、能读回，不对就直接退出；每步写完 PNG 与 jsonl 都读回校验大小，页面上显示写到了哪个绝对路径。网页里：下拉框选 App 即打开并开始录制；选已录的 case 点“回放”，逐步显示 ✓/✗、差异比例与录制/回放两张截图（回放截图在 `e2e/out/web/`）。

```jsonl
{"v":2,"pkg":"com.android.settings","device":"…","sdk":35,"w":1080,"h":2640,"dpi":420,"created":"…","wait":{"change":0.005,"settle_ms":500,"timeout_ms":3000,"ignore":[[834,878]]}}
{"Open":"com.android.settings","res":{"tChange":94,"tSettle":0,"rows":2231,"volatile":[58,2640]},"stack":[1,"com.android.settings/.Settings"],"wait":{"settle_ms":700,"timeout_ms":700}}
{"Tap":"@540,1066","sel":{"id":"…:id/text_frame","cls":"…","b":[147,958,1080,1174]},"res":{"tChange":44,"tSettle":918,"rows":1655},"stack":[2,"…/.SubSettings"]}
{"Swipe":["@540,2000","@540,1000"],"pts":[[0,540,2000],[30,540,1950],…],"res":{"tChange":13,"tSettle":1153,"rows":2436}}
{"Back":true,"res":{…},"stack":[1,"…/.Settings"],"wait":{"ignore":[[2298,2415]],"reignored":true}}
```

**schema 约定（不过度规范）**

- **行号推定**：第 1 行是 meta；之后第 i 行就是第 i 步（从 0 起）。不写 `type`、`i`、`shot`；截图是 `<用例名>/NNNN.png`（NNNN = 步号 4 位）。
- **事件用键名**：`Open`（包名）、`Tap`（`"@x,y"`）、`Long`（`"@x,y"` + `ms`）、`Swipe`（`["@x1,y1","@x2,y2"]` + 回放用的轨迹 `pts`）、`Back`。
- **`res`**：`tChange` / `tSettle` 是毫秒，**`== 0` 表示没发生**：`tChange == 0` = 没有变化，`tSettle == 0` = 超时没静止。没有单独的 `changed` / `settled` 字段，读取时用 `== 0` 推。`volatile` 是超时时一直在变的行带。
- **`wait`** 只写和 meta 的 `wait` 不同的键（open 步骤固定 700ms，所以总有 `settle_ms` / `timeout_ms`）；读取时 `{...meta.wait, ...step.wait}`。
- **`reignored`**：该步骤的忽略是录制之后加的，回放时不再比较它的“有无变化”（录制时的变化是在没有这段忽略时算的）。自动跑（`run.mjs`）同样遵守，不会因此假报警。
- **`stack`** 是 `[栈深, 栈顶 Activity]`。

- **保存**：录制先写到 `e2e/cases/.draft/`，点网页的“保存此重录”弹出 `prompt()`：填用例名 → 存成 `<用例名>.jsonl` + `<用例名>/`；留空 → 覆盖原来的 `<包名>.jsonl`。用例名不能含 `/ \`、不能以 `.` 开头。`e2e/cases/*/` 下的截图在 `.gitignore` 里，只有 jsonl 入库。
- **`open` 步骤**：不等静止帧，固定等 700ms 就截图；回放时它的画面差异只作说明、不判失败，也不参与 `--repeat` 的次间比较。
- **事件**：`Open`、`Tap`、`Long`、`Swipe`（带重采样后的轨迹）、`Back`（右键 / Esc）。
- **等待（默认）**：每个事件注入后，等“相对注入前的画面有 ≥ `change`×高度 行变化”→ 再等“`settle_ms` 内没有新的行变化”→ 最长 `timeout_ms`；然后截图（PNG，无损）。`res` 记录变化 / 静止的耗时，`settled:false` 表示超时没静止。
- **`ignore` 行带**：走马灯、转圈这类一直在变的区域。录制时超时未静止会提示变化集中的行范围，一键忽略；等待与比对都跳过这些行。
- **`sel`**（点按带）：点按时点中的节点（resource-id / 文本 / 描述 / 行号 / 屏幕矩形）。回放时重新在节点树里找到它：布局没变用原坐标，变了按节点相对位置换算，找不到就用原坐标并提示。
- **手势**：从按下到松手是一个整体——位移不超过 48 设备像素算点按（按住超过 0.5s 为长按），一旦超过就是从按下到松手的整段拖动。
- **手势确定性**：浏览器只在本地收集轨迹，松手时一次性交给 host 按时间轴注入；录制与回放走同一份 `lib/steps.mjs`。轨迹被重采样成 30ms 间隔、每步位移 ≤ 60px（更快的滑动在 Android 上结果每次不同，实测 107px/步 两次回放差 5–7%，50px/步逐像素一致）；滑动抬手前在终点停 60ms，所以没有惯性。
- **回放**：`node e2e/run.mjs` 逐步注入 → 同样的等待 → 截图 → 与录制的 PNG 比对（不同像素占比 ≤ `--tol`，默认 0.1%），另查“有无变化”和栈深。`--repeat N` 额外比较各次之间的截图。

## 网页行为补充

- 录制的 App 写在 URL hash（`#包名`）：刷新页面时，服务端草稿还在就原样恢复步骤列表（不重开 App）；没有草稿就按 hash 重新打开。
- 一划 = 左键按下到弹起；弹起之后（等待静止期间）光标移动不再画线。
- 回放时右上有“回放中 i/N”；即时画面送不出来超过 1.5s 就显示上一步的截图。
- 回放里失败的步骤有“忽略 y a–b（仅此步）”按钮：把那段行带写进**这一步**的 `wait.ignore`，并标 `reignored`。区域超过屏幕高度一半只提示（这一步画面本身不确定，可考虑换页面），照常写入。
- 回放点按前，若步骤带选择器，先等目标节点出现（最多 8s）：`open` 只等固定 700ms，而录制时人是等首页出来才点的。

## 已知限制

- 内容来自网络的页面（如新闻 App）每次不同，回放必然不一致，只能换页面。

- 设置首页建议卡片有走马灯文字（一直在动）：录制时会提示忽略行带；它位置变了（卡片消失）后需要重录。
- 应用异步加载（例如“网络与互联网”页某行摘要晚到 >300ms）会让静止判定过早：把“静止”调到 500ms 以上。
- 场景 runner 的 `tap {first:true}` 取“主列表里最上面的可点节点”：设置首页会落到建议卡片上，点一次状态就变，偶尔导致 `enter-back` 失败；这类页面用人工录制（带选择器）更稳。
- `scroll-down-up` 目前只在首屏有竖直滚动容器、且响应滚轮的 App 上跑（P1：libchecker 精确，设置首页 / activitylauncher 近似）；其余 App 报“跳过”并写明原因。

## 实跑模式（`scenario.mjs --live`）

`node e2e/scenario.mjs cold-open --live`：runner 扮演 A，经调试口 `wire` 发 Up；B 的编码线程把 Down 写进进程内的“虚拟 A”（`host-net/src/va.rs`）用 rd 解码；性质在 **A 的画布**上查（`mark` 取的是 A 画布的每行 hash），另外每个标记点和结束时查：A 画布与 B 当前帧逐像素一致（有损图片除外）、`Verify` 零失败。输出带首屏字节和耗时。
- 通用场景会点进任意子页，所以默认放行 `[popup]`；场景可用 `policy` 字段覆盖。
- 某步让 App 退到根页之外（根页 Back → `AppGone`，会话按策略结束）时该场景标“跳过”。
- `scroll-down-up` 跳过（本地滚动是 M2）。设置、Aggregator News、SAI 的 `enter-back` 通过；LibChecker、圣经、ActivityLauncher、NewPipe、QPython 在**非实跑模式下同样不过**（返回后画面与进入前不同，是 App 自身的不确定，不是编解码：A 与 B 逐像素一致），沿用 M0 的结论。

