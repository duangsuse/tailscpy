//! 编码实验：`cargo run --release -p rd --example lab -- frame.rgba W H [k=v …]`，打印各 Op 种类的字节与耗时。
use rd::enc::Encoder; use rd::params::Params; use rd::wire::*;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (w, h): (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let f = std::fs::read(&a[1]).unwrap(); let mut p = Params::new();
    for kv in &a[4..] { let (k, v) = kv.split_once('=').unwrap(); p.set(k, v.parse().unwrap()); }
    let mut e = Encoder::new(w, h); e.push_frame(&f, w * 4);
    let t = std::time::Instant::now(); let o = e.encode(&p, 0); let ms = t.elapsed().as_secs_f64() * 1e3;
    let mut by: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for op in &o.ops { let n = to_postcard(op).len(); let x = by.entry(op.kind()).or_default(); x.0 += n; x.1 += 1 }
    let tot: usize = by.values().map(|x| x.0).sum();
    println!("{} 字节  {:.0} ms  {:?}", tot, ms, by);
}
