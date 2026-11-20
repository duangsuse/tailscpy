//! WebTransport 服务端（50 §1、§3、§5）。证书：B 自签 ECDSA P-256，有效期 < 14 天（浏览器 `serverCertificateHashes` 的要求），落盘复用；
//! 鉴权：`Hello.token`（证书 hash 不是秘密）。流：A 开 `ctl`（双向）和 `up`（单向，A→B）；B 开 `tx`（单向，B→A，画面数据，一条有序流）。
//! UDP 载荷固定 1200 并关掉路径 MTU 探测（Tailscale MTU 1280）。

use crate::core::{Conn, Core};
use rd::wire::*;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use wtransport::config::QuicTransportConfig;
use wtransport::tls::{Certificate, CertificateChain, PrivateKey};
use wtransport::{Endpoint, Identity, ServerConfig, VarInt};

pub const CERT_MAX_AGE_S: u64 = 10 * 86400;

pub fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

fn now_s() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

pub fn random_token() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") { let _ = f.read_exact(&mut b); }
    hex(&b)
}

/// notBefore 取一天前：A 和 B 的时钟差几分钟也不会“证书还没生效”（有效期总长 13 天 < 浏览器要求的 14 天）。
/// 读（或新建）证书与 token。证书超过 `CERT_MAX_AGE_S` 换新（URL 会变，不做轮换协议）。返回 (identity, cert hash hex, token, 新建了证书?)
pub fn load_identity(dir: &Path, sans: &[String]) -> Result<(Identity, String, String, bool), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {dir:?}: {e}"))?;
    let (cert_f, key_f, time_f, tok_f) = (dir.join("cert.der"), dir.join("key.der"), dir.join("cert.time"), dir.join("token"));
    let age = std::fs::read_to_string(&time_f).ok().and_then(|s| s.trim().parse::<u64>().ok()).map(|t| now_s().saturating_sub(t));
    let mut fresh = false;
    let ident = match (std::fs::read(&cert_f), std::fs::read(&key_f), age) {
        (Ok(c), Ok(k), Some(a)) if a < CERT_MAX_AGE_S => {
            Identity::new(CertificateChain::single(Certificate::from_der(c).map_err(|e| e.to_string())?), PrivateKey::from_der_pkcs8(k))
        }
        _ => {
            fresh = true;
            let id = Identity::self_signed_builder().subject_alt_names(sans.iter().map(|s| s.as_str())).not_before(time::OffsetDateTime::now_utc() - time::Duration::days(1)).validity_days(13).build().map_err(|e| e.to_string())?;
            std::fs::write(&cert_f, id.certificate_chain().as_slice()[0].der()).map_err(|e| e.to_string())?;
            std::fs::write(&key_f, id.private_key().secret_der()).map_err(|e| e.to_string())?;
            std::fs::write(&time_f, now_s().to_string()).map_err(|e| e.to_string())?;
            id
        }
    };
    let hash = hex(ident.certificate_chain().as_slice()[0].hash().as_ref());
    let token = match std::fs::read_to_string(&tok_f) { Ok(t) if t.trim().len() >= 16 => t.trim().to_string(), _ => { let t = random_token(); std::fs::write(&tok_f, &t).map_err(|e| e.to_string())?; t } };
    Ok((ident, hash, token, fresh))
}

pub fn endpoint(addr: SocketAddr, ident: Identity) -> Result<Endpoint<wtransport::endpoint::endpoint_side::Server>, String> {
    let mut tc = QuicTransportConfig::default();
    tc.initial_mtu(1200).mtu_discovery_config(None).keep_alive_interval(Some(Duration::from_secs(5)))
        .max_idle_timeout(Some(Duration::from_secs(30).try_into().unwrap()));
    let cfg = ServerConfig::builder().with_bind_address(addr).with_custom_transport(ident, tc).build();
    Endpoint::server(cfg).map_err(|e| format!("绑 {addr}: {e}"))
}

pub async fn serve(ep: Endpoint<wtransport::endpoint::endpoint_side::Server>, core: Arc<Core>) {
    loop {
        let inc = ep.accept().await;
        let core = core.clone();
        tokio::spawn(async move {
            let Ok(req) = inc.await else { return };
            let Ok(conn) = req.accept().await else { return };
            if let Err(e) = session(conn, core.clone()).await { core.up_event(format!("{{\"ev\":\"conn_error\",\"msg\":{}}}", serde_json::to_string(&e).unwrap())) }
        });
    }
}

async fn read_msgs(mut r: wtransport::RecvStream, mut on: impl FnMut(Up) -> bool) -> Result<(), String> {
    let mut de = Deframer::default(); let mut buf = vec![0u8; 16384];
    loop {
        let n = match r.read(&mut buf).await.map_err(|e| e.to_string())? { Some(n) => n, None => return Ok(()) };
        de.push(&buf[..n]);
        while let Some(m) = de.next()? { let up: Up = from_postcard(&m)?; if !on(up) { return Ok(()) } }
    }
}

async fn session(conn: wtransport::Connection, core: Arc<Core>) -> Result<(), String> {
    // 握手：A 在 ctl 上先发 Hello
    let (mut ctl_send, mut ctl_recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
    let hello = {
        let mut de = Deframer::default(); let mut buf = vec![0u8; 4096];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(10), ctl_recv.read(&mut buf)).await.map_err(|_| "等 Hello 超时".to_string())?
                .map_err(|e| e.to_string())?.ok_or("连接在 Hello 之前关闭")?;
            de.push(&buf[..n]);
            if let Some(m) = de.next()? { break from_postcard::<Up>(&m)? }
        }
    };
    let Up::Hello { ver, token, .. } = hello else { return Err("第一条不是 Hello".into()) };
    let fault = |code: FaultCode, msg: &str| frame(&Down::Fault { code, msg: msg.into() });
    if ver != VER { let _ = ctl_send.write_all(&fault(FaultCode::BadVersion, &format!("B 版本 {VER}，A 版本 {ver}"))).await; conn.close(VarInt::from_u32(1), b"bad version"); return Ok(()) }
    if token != core.token { let _ = ctl_send.write_all(&fault(FaultCode::BadToken, "token 不对")).await; conn.close(VarInt::from_u32(2), b"bad token"); return Ok(()) }

    // 新连接顶掉旧连接（50 §3）
    let id = core.conn_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let (tx_tx, mut tx_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let old = core.conn.lock().unwrap().replace(Conn { id, ctl: ctl_tx.clone(), tx: tx_tx, conn: Some(conn.clone()) });
    if let Some(o) = old { let _ = o.ctl.send(frame(&Down::Replaced)); if let Some(oc) = o.conn.clone() { tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(300)).await; oc.close(VarInt::from_u32(3), b"replaced") }); } }
    core.reset_flow();

    // 缓存对齐 M1 固定回 Reset（任务 11）
    let apps = core.apps.lock().unwrap().clone(); let params = core.params.lock().unwrap().table();
    let _ = ctl_tx.send(frame(&Down::Welcome { ver: VER, cache: CacheMode::Reset, apps, params }));
    core.up_event(format!("{{\"ev\":\"connected\",\"conn\":{id},\"peer\":\"{}\"}}", conn.remote_address()));

    // tx 流（B→A，一条有序流）；ctl 的优先级高于 tx（50 §5）
    let mut tx_send = conn.open_uni().await.map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
    ctl_send.set_priority(10); tx_send.set_priority(0);
    let c2 = conn.clone();
    let w_tx = tokio::spawn(async move { while let Some(b) = tx_rx.recv().await { if tx_send.write_all(&b).await.is_err() { break } } c2.close(VarInt::from_u32(0), b"") });
    let w_ctl = tokio::spawn(async move { while let Some(b) = ctl_rx.recv().await { if ctl_send.write_all(&b).await.is_err() { break } } });

    // 每秒统计
    let (core_s, stop) = (core.clone(), conn.clone());
    let w_stats = tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_secs(1));
        let _ = &stop;
        loop { iv.tick().await; if core_s.conn.lock().unwrap().as_ref().map(|c| c.id) != Some(id) { break } core_s.send_ctl(&core_s.stats_down()); }
    });

    // 上行：ctl 和 up 两条流同一个处理函数
    let handle = {
        let core = core.clone(); let conn = conn.clone();
        move |up: Up| -> bool {
            match up {
                Up::Ack { seq } => core.on_ack(seq),
                Up::Ping { t } => { core.send_ctl(&Down::Pong { t }); }
                Up::Tune { scope, key, val } => {
                    let ok = core.params.lock().unwrap().set(&key, val);
                    if ok { let t = core.params.lock().unwrap().table(); core.send_ctl(&Down::Params { params: t }); core.kick() }
                    let _ = scope;
                }
                Up::Probe { bytes } => {
                    let conn = conn.clone();
                    tokio::spawn(async move {
                        if let Ok(op) = conn.open_uni().await { if let Ok(mut s) = op.await {
                            let chunk = vec![0xA5u8; 16384]; let mut left = bytes as usize;
                            while left > 0 { let n = left.min(chunk.len()); if s.write_all(&chunk[..n]).await.is_err() { return } left -= n }
                            let _ = s.finish().await;
                        } }
                    });
                }
                Up::View { y, .. } => core.set_view(y),
                Up::Hello { .. } => {}
                other => core.up_event(format!("{{\"ev\":\"up\",\"msg\":{}}}", to_json(&other))),
            }
            true
        }
    };
    let h2 = handle.clone();
    let r_ctl = tokio::spawn(read_msgs(ctl_recv, handle));
    let c3 = conn.clone();
    let r_up = tokio::spawn(async move {
        loop { match c3.accept_uni().await { Ok(r) => { let h = h2.clone(); tokio::spawn(read_msgs(r, h)); } Err(_) => break } }
    });

    let _ = conn.closed().await;
    for t in [&w_tx, &w_ctl, &w_stats, &r_up] { t.abort() }
    r_ctl.abort();
    let mut g = core.conn.lock().unwrap();
    if g.as_ref().map(|c| c.id) == Some(id) { *g = None; drop(g); core.up_event(format!("{{\"ev\":\"disconnected\",\"conn\":{id}}}")); }
    Ok(())
}
