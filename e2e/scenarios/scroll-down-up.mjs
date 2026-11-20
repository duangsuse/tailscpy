// 主列表往下滚若干屏，再滚回顶。性质：回到顶后画面与初始一致（回程没有像素字节由 bench 查）。
export default { name: 'scroll-down-up', needs: 'list',
  steps: [['open'], ['waitStable'], ['mark', 'top'], ['scrollBy', 2400], ['mark', 'down'], ['scrollBy', -2400], ['mark', 'up'], ['waitStable']],
  // 实跑模式另查：回程零像素流量（滚回已见内容不重发；允许一点点：标题栏折叠 / 展开的几行）
  check: r => !r.same('top', 'up') ? '滚回顶后画面与初始不一致'
    : r.live && r.marks.up.rx - r.marks.down.rx > 40000 ? `回程下行 ${r.marks.up.rx - r.marks.down.rx} 字节（应接近 0）` : null }
