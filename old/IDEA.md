活镜App用户体验开发 (A->B点滑; A<-B推流)

- 依赖 Tailscale和 Shizuku-Next，前者用于方便连接，后者用于模块运行
- B端可选开放哪些App，A端也可随时重创建快捷方式，或在网格widget中置顶任何App
- 支持桌面小部件和快捷方式创建，在衔接有效时，旧图被用于优化TTFP，同时也在管理App的 main grid 预览和【长按图集】中出现
- 支持作为文件选择器（走全部文件访问权限/Shizuku自动开，或SAF），注册文件分享菜单【分享到设备/Sharing Shortcuts】【从设备拉到同目录】

补充
- 文件选择器UI代码不能与安卓耦合，需要不开屏幕共享也能用，需要在 miniserve; py -m http.server 上测试backend的通用性
- 高版本后台需要做保活，“活跃运行的应用”中可出现，通知中尽量避免。只可开启灵动岛
- 注意，实现方式是推流为底，但每帧图片和文本尽量擦掉，扣出去。图标池化 （已废弃）
- 以及 main grid 【长按文集】的不重复文本捕获，【最近图标池】，【消息集】的 Chat-Msg-Txt date 的A11y树投影DOM+JS提取规则设计（可参考Appium但只用于统一协议和调试时）
- 第三部分会做APK分身，可供参考：SAI、Taichi、 https://github.com/duangsuse/Pinata/blob/main/apksigner.js#L34
- 【可以做的假设】局域网15ms延迟、B屏常闭、App重点在守护进程
