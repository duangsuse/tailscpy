// 计算器连续按键。性质：按下高亮 / 涟漪只是 Overlay，不污染画布 —— 清除之后画面回到初始（实跑模式另查：A 画布与 B 帧一致）。
export default { name: 'taps-in-place', needs: 'calculator', pkgs: ['com.miui.calculator'],
  steps: [['open'], ['waitStable'], ['mark', 'r0'],
    ['tap', { desc: '加' }], ['tap', { desc: '减' }], ['tap', { desc: '乘' }], ['tap', { desc: '除' }], ['tap', { desc: '加' }], ['tap', { desc: '减' }],
    ['waitStable'], ['mark', 'pressed'], ['tap', { desc: '清除' }], ['waitStable'], ['mark', 'cleared']],
  // 实跑模式的性质由 runner 统一查：每个标记点 A 的画面（画布 + Overlay）与 B 的帧逐像素一致、Verify 零失败。
  // “清除后等于初始”只在非实跑模式查：MIUI 计算器按过运算符后表达式 / 历史区的状态本身就不等于初始（App 语义，不是编解码）。
  check: r => r.live || r.same('r0', 'cleared') ? null : '清除之后画面与初始不一致' }
