// 主列表往下滚若干屏，再滚回顶。性质：回到顶后画面与初始一致（回程没有像素字节由 bench 查）。
export default { name: 'scroll-down-up', needs: 'list',
  steps: [['open'], ['waitStable'], ['mark', 'top'], ['scrollBy', 2400], ['mark', 'down'], ['scrollBy', -2400], ['mark', 'up'], ['waitStable']],
  check: r => r.same('top', 'up') ? null : '滚回顶后画面与初始不一致' }
