// 打开 App，等稳定。性质：画面非空白；记首屏稳定耗时。
export default { name: 'cold-open', steps: [['open'], ['waitStable'], ['mark', 'first-screen']],
  check: r => r.final && !r.final.blank ? null : '首屏是空白帧' }
