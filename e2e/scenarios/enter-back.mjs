// 进子页 → 返回，重复三轮。性质：每轮返回后画面都等于进入前（第二轮起没有像素字节由 bench 查）。
export default { name: 'enter-back', needs: 'clickable',
  steps: [['open'], ['waitStable'], ['mark', 'r0'],
    ['tap', { first: true }], ['waitStable'], ['mark', 'in1'], ['back'], ['mark', 'r1'],
    ['tap', { first: true }], ['waitStable'], ['mark', 'in2'], ['back'], ['mark', 'r2'],
    ['tap', { first: true }], ['waitStable'], ['mark', 'in3'], ['back'], ['mark', 'r3']],
  check: r => ['r1', 'r2', 'r3'].every(k => r.same('r0', k)) && r.same('in1', 'in2') && r.same('in1', 'in3') ? null : '返回后画面与进入前不一致，或各轮子页不一致' }
