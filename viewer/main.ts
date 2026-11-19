// viewer 骨架：加载 rd.wasm 并打印版本。合成 / 输入 / OPFS / 调试面板在 M1 起加。
const res = await fetch('./rd.wasm')
const { instance } = await WebAssembly.instantiate(await res.arrayBuffer(), {})
document.getElementById('o')!.textContent = `rd.wasm 已加载，导出：${Object.keys(instance.exports).join(', ') || '(无)'}`
