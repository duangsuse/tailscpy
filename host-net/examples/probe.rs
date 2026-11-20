//! 命令行 A：`cargo run -p hj_host --example probe -- <地址:端口> <证书hash> <token>`。握手并打印 Welcome，用来区分“浏览器问题”和“服务端问题”。
use rd::wire::*;
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Endpoint};
#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let hash: [u8; 32] = (0..32).map(|i| u8::from_str_radix(&a[2][i * 2..i * 2 + 2], 16).unwrap()).collect::<Vec<_>>().try_into().unwrap();
    let cfg = ClientConfig::builder().with_bind_default().with_server_certificate_hashes([Sha256Digest::new(hash)]).build();
    let ep = Endpoint::client(cfg).unwrap();
    let conn = match ep.connect(format!("https://{}/", a[1])).await { Ok(c) => c, Err(e) => { eprintln!("连接失败: {e:?}"); std::process::exit(1) } };
    let (mut s, mut r) = conn.open_bi().await.unwrap().await.unwrap();
    s.write_all(&frame(&Up::Hello { ver: VER, token: a[3].clone(), cache_id: "probe".into(), cache_rev: 0 })).await.unwrap();
    let mut buf = vec![0u8; 65536]; let mut de = Deframer::default();
    loop { let n = r.read(&mut buf).await.unwrap().unwrap(); de.push(&buf[..n]);
        if let Some(m) = de.next().unwrap() { let d: Down = from_postcard(&m).unwrap(); println!("{}", if a.len() > 4 { format!("{:?}", std::mem::discriminant(&d)) } else { to_json(&d) }); break } }
    // 第 4 个参数：包名 → 打开会话，保持 5 秒后直接断开（不发 Close），用来验证断线超时后 B 还原设置
    if let Some(pkg) = a.get(4) {
        s.write_all(&frame(&Up::Open { pkg: pkg.clone(), geom: None })).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        println!("断开（不发 Close）");
    }
}
