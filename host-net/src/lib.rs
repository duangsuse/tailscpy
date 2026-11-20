//! host-net：和 rd 链进同一个 libhj_host.so。Kotlin 经 JNI 调用（`hj.Native`）：
//!   - 网络：`start` 起 WebTransport；上行消息和连接事件用 `nextUp` 阻塞取（JSON，Up 的 JSON 形式）；控制类下行用 `down`（Down 的 JSON 形式）
//!   - 画面：`begin` 开会话，`frame` 推最新一帧（只拷贝，编码在独立线程），`hints` 给 A11y 图片矩形
//! 约定：Kotlin 单向调进来，Rust 不回调 Kotlin（不用缓存 JavaVM / 类引用）。

pub mod core;
pub mod net;
pub mod va;

use crate::core::Core;
use jni::objects::{JByteArray, JClass, JString, ReleaseMode};
use jni::sys::{jboolean, jdouble, jint, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use rd::wire::*;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

static CORE: OnceLock<Arc<Core>> = OnceLock::new();

fn s(env: &mut JNIEnv, j: &JString) -> String { env.get_string(j).map(|x| x.into()).unwrap_or_default() }
fn js(env: &JNIEnv, v: &str) -> jstring { env.new_string(v).expect("new_string").into_raw() }
fn core() -> Option<&'static Arc<Core>> { CORE.get() }

#[no_mangle]
pub extern "system" fn Java_hj_Native_version(env: JNIEnv, _c: JClass) -> jstring { js(&env, rd::version()) }

/// `start(dir, binds, sans)`：binds = "ip:port,ip:port"；sans = 证书 SAN 列表（逗号分隔）。返回 JSON。
#[no_mangle]
pub extern "system" fn Java_hj_Native_start(mut env: JNIEnv, _c: JClass, dir: JString, binds: JString, sans: JString) -> jstring {
    let (dir, binds, sans) = (s(&mut env, &dir), s(&mut env, &binds), s(&mut env, &sans));
    let r = (|| -> Result<String, String> {
        if CORE.get().is_some() { return Err("已经启动".into()) }
        let sans: Vec<String> = sans.split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect();
        let (ident, hash, token, fresh) = net::load_identity(Path::new(&dir), &sans)?;
        let core = Core::new(token.clone());
        let addrs: Vec<SocketAddr> = binds.split(',').filter(|x| !x.is_empty()).map(|x| x.parse::<SocketAddr>().map_err(|e| format!("{x}: {e}"))).collect::<Result<_, _>>()?;
        if addrs.is_empty() { return Err("没有可绑定的地址".into()) }
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("hj-net").enable_all().build().map_err(|e| e.to_string())?;
        let mut eps = vec![];
        { let _g = rt.enter(); for a in &addrs { eps.push(net::endpoint(*a, ident.clone_identity())?) } }
        for ep in eps { rt.spawn(net::serve(ep, core.clone())); }
        let c2 = core.clone(); std::thread::Builder::new().name("hj-enc".into()).spawn(move || crate::core::encoder_loop(c2)).map_err(|e| e.to_string())?;
        std::thread::Builder::new().name("hj-rt".into()).spawn(move || { rt.block_on(std::future::pending::<()>()) }).map_err(|e| e.to_string())?;
        let _ = CORE.set(core);
        Ok(serde_json::json!({"ok": true, "hash": hash, "token": token, "fresh": fresh, "binds": addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>()}).to_string())
    })();
    js(&env, &r.unwrap_or_else(|e| serde_json::json!({"ok": false, "err": e}).to_string()))
}

/// 阻塞取下一条上行消息 / 连接事件（JSON 一行）；超时返回 null
#[no_mangle]
pub extern "system" fn Java_hj_Native_nextUp(env: JNIEnv, _c: JClass, timeout_ms: jint) -> jstring {
    let Some(c) = core() else { std::thread::sleep(Duration::from_millis(timeout_ms.max(0) as u64)); return std::ptr::null_mut() };
    let rx = c.up_rx.lock().unwrap();
    match rx.recv_timeout(Duration::from_millis(timeout_ms.max(0) as u64)) { Ok(m) => js(&env, &m), Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => std::ptr::null_mut() }
}

/// 控制类下行（Welcome 之外的 Opened / Focus / Clip / Fault / Params / Purge…），Down 的 JSON 形式
#[no_mangle]
pub extern "system" fn Java_hj_Native_down(mut env: JNIEnv, _c: JClass, json: JString) -> jboolean {
    let j = s(&mut env, &json);
    match (core(), from_json::<Down>(&j)) { (Some(c), Ok(d)) => if c.send_ctl(&d) { JNI_TRUE } else { JNI_FALSE }, _ => JNI_FALSE }
}

/// 本机白名单 App 列表（AppInfo 数组的 JSON），下次握手的 Welcome 带上
#[no_mangle]
pub extern "system" fn Java_hj_Native_setApps(mut env: JNIEnv, _c: JClass, json: JString) -> jboolean {
    let j = s(&mut env, &json);
    match (core(), from_json::<Vec<AppInfo>>(&j)) { (Some(c), Ok(a)) => { *c.apps.lock().unwrap() = a; JNI_TRUE } _ => JNI_FALSE }
}

#[no_mangle]
pub extern "system" fn Java_hj_Native_begin(_env: JNIEnv, _c: JClass, w: jint, h: jint) { if let Some(c) = core() { c.begin(w as usize, h as usize) } }

#[no_mangle]
pub extern "system" fn Java_hj_Native_end(_env: JNIEnv, _c: JClass) { if let Some(c) = core() { c.end() } }

/// 推最新一帧：整帧 RGBA，行距 = w*4。只做内存拷贝，编码在 hj-enc 线程。
#[no_mangle]
pub extern "system" fn Java_hj_Native_frame(mut env: JNIEnv, _c: JClass, buf: JByteArray) {
    let Some(c) = core() else { return };
    let Ok(arr) = (unsafe { env.get_array_elements_critical(&buf, ReleaseMode::NoCopyBack) }) else { return };
    {
        let mut f = c.frame.lock().unwrap();
        if f.buf.len() != arr.len() { return }   // 尺寸对不上（会话切换中）：丢
        let src = unsafe { std::slice::from_raw_parts(arr.as_ptr() as *const u8, arr.len()) };
        f.buf.copy_from_slice(src);
    }
    drop(arr);
    c.notify_frame();
}

/// A11y 图片矩形提案：`[[x,y,w,h],…]`
#[no_mangle]
pub extern "system" fn Java_hj_Native_hints(mut env: JNIEnv, _c: JClass, json: JString) {
    let j = s(&mut env, &json);
    let Some(c) = core() else { return };
    let Ok(v) = serde_json::from_str::<Vec<[u32; 4]>>(&j) else { return };
    if let Some(e) = c.enc.lock().unwrap().as_mut() { e.set_hints(v.into_iter().map(|r| Rect { x: r[0], y: r[1], w: r[2], h: r[3] }).collect()) }
    c.kick();   // 提案晚到：已按无损发出的照片要改发 JPEG，压后的条带也可以放行了
}

/// A 的视口（画布 y）：调试口 `wire View` 走这里；网络上来的 View 在 net.rs 里直接处理
#[no_mangle]
pub extern "system" fn Java_hj_Native_view(_env: JNIEnv, _c: JClass, y: jint) { if let Some(c) = core() { c.set_view(y) } }

/// A11y 的 Scroll 区域提案：`[x,y,w,h]` 或 `[]`
#[no_mangle]
pub extern "system" fn Java_hj_Native_scrollRegion(mut env: JNIEnv, _c: JClass, json: JString) {
    let j = s(&mut env, &json);
    let Some(c) = core() else { return };
    let v = serde_json::from_str::<Vec<u32>>(&j).unwrap_or_default();
    let r = Some(v.as_slice()).filter(|v| v.len() >= 4).map(|v| Rect { x: v[0], y: v[1], w: v[2], h: v[3] });
    let dirs = v.get(4).map(|f| (f & 1 != 0, f & 2 != 0));
    if let Some(e) = c.enc.lock().unwrap().as_mut() { e.set_scroll_proposal(r); e.set_scroll_dirs(dirs) }
    c.kick();
}

/// 点击换算：内容坐标 y → 屏幕 y，并做点前校验。返回 JSON
#[no_mangle]
pub extern "system" fn Java_hj_Native_resolveTap(env: JNIEnv, _c: JClass, y: jint, wait_ms: jint) -> jstring {
    match core() { Some(c) => js(&env, &c.resolve_tap(y as i64, wait_ms.max(0) as u64).to_string()), None => js(&env, "{\"ok\":false,\"why\":\"nonet\"}") }
}

#[no_mangle]
pub extern "system" fn Java_hj_Native_param(mut env: JNIEnv, _c: JClass, key: JString, val: jdouble) -> jboolean {
    let k = s(&mut env, &key);
    match core() { Some(c) => { let ok = c.params.lock().unwrap().set(&k, val); if ok { c.kick() } if ok { JNI_TRUE } else { JNI_FALSE } } None => JNI_FALSE }
}

/// 最新一帧是否纯色（BlankFrame 判定用；看推进来的帧，不依赖是否有连接）
#[no_mangle]
pub extern "system" fn Java_hj_Native_blank(_env: JNIEnv, _c: JClass) -> jboolean {
    let Some(c) = core() else { return JNI_FALSE };
    // 锁顺序：enc → frame（编码线程同序）；这里反过来取会死锁
    if c.enc.lock().unwrap().is_none() { return JNI_FALSE }
    let f = c.frame.lock().unwrap();
    if f.buf.len() < 8 { return JNI_FALSE }
    let first = &f.buf[..4];
    if f.buf.chunks_exact(4).all(|p| p == first) { JNI_TRUE } else { JNI_FALSE }
}

/// 统计快照（JSON）：调试口和 e2e 用
#[no_mangle]
pub extern "system" fn Java_hj_Native_stats(env: JNIEnv, _c: JClass) -> jstring {
    let Some(c) = core() else { return js(&env, "{}") };
    let req = { let d = c.dbg_req.lock().unwrap(); serde_json::json!([d.0, d.1, d.2, d.3]) };
    let st = c.stats.lock().unwrap();
    let j = serde_json::json!({
        "layers": {"bg": st.layers[0], "lit": st.layers[1], "fg": st.layers[2]},
        "by_kind": st.by_kind.iter().map(|(k, v)| (k.clone(), *v)).collect::<std::collections::BTreeMap<_, _>>(),
        "frames": st.frames, "rows": st.rows_sent, "tx_bytes": st.tx_bytes, "tx_msgs": st.tx_msgs,
        "enc_ms_last": st.enc_ms_last, "enc_ms_total": st.enc_ms_total, "inflight": c.inflight(),
        "connected": c.conn.lock().unwrap().is_some(),
        "wheel_f": *c.wheel_f.lock().unwrap(), "req": req, "view": *c.view.lock().unwrap(),
        "scroll": c.enc.lock().unwrap().as_ref().and_then(|e| e.scroll_head().map(|(h, r)| serde_json::json!({"head": h, "rect": [r.x, r.y, r.w, r.h], "edges": e.scroll_edges(), "n_shift": e.n_shift, "n_reset": e.n_reset, "n_unexplained": e.n_unexplained, "rows_new": e.n_rows_new, "rows_changed": e.n_rows_changed, "n_pin": e.n_pin, "last_unexplained": e.last_unexplained, "log": e.dbg_log.iter().collect::<Vec<_>>()}))),
    });
    js(&env, &j.to_string())
}

/// wire 类型 JSON → postcard → JSON 往返（调试口 `wire_rt` 用，验证两种形式等价）。kind = "Up" | "Down"
#[no_mangle]
pub extern "system" fn Java_hj_Native_roundtrip(mut env: JNIEnv, _c: JClass, kind: JString, json: JString) -> jstring {
    let (k, j) = (s(&mut env, &kind), s(&mut env, &json));
    let r: Result<serde_json::Value, String> = (|| {
        let (bytes, back) = match k.as_str() {
            "Up" => { let m: Up = from_json(&j)?; let b = to_postcard(&m); let back = to_json(&from_postcard::<Up>(&b)?); (b, back) }
            "Down" => { let m: Down = from_json(&j)?; let b = to_postcard(&m); let back = to_json(&from_postcard::<Down>(&b)?); (b, back) }
            _ => return Err(format!("kind 要么 Up 要么 Down: {k}")),
        };
        Ok(serde_json::json!({"len": bytes.len(), "hex": net::hex(&bytes), "json": serde_json::from_str::<serde_json::Value>(&back).unwrap_or_default()}))
    })();
    js(&env, &r.unwrap_or_else(|e| serde_json::json!({"err": e})).to_string())
}

/// 虚拟 A（实跑模式）：开 / 关
#[no_mangle]
pub extern "system" fn Java_hj_Native_vaAttach(_env: JNIEnv, _c: JClass, on: jboolean) { if let Some(c) = core() { if on != 0 { va::attach(c) } else { va::detach(c) } } }

/// 虚拟 A 的状态（JSON）；rows=true 时带每行 hash
#[no_mangle]
pub extern "system" fn Java_hj_Native_vaState(env: JNIEnv, _c: JClass, rows: jboolean) -> jstring {
    match core() { Some(c) => js(&env, &va::state(c, rows != 0).to_string()), None => js(&env, "{\"attached\":false}") }
}

/// 调试：虚拟 A 当前画面写成裸 RGBA 文件，返回字节数（或错误文本）
#[no_mangle]
pub extern "system" fn Java_hj_Native_vaDump(mut env: JNIEnv, _c: JClass, path: JString) -> jstring {
    let p = s(&mut env, &path);
    match core() { Some(c) => js(&env, &match va::dump(c, &p) { Ok(n) => n.to_string(), Err(e) => format!("err:{e}") }), None => js(&env, "err:nonet") }
}

/// 调试：虚拟 A 最近收到的 Tx 的 Op 摘要（JSON 数组）
#[no_mangle]
pub extern "system" fn Java_hj_Native_vaLog(env: JNIEnv, _c: JClass) -> jstring { match core() { Some(c) => js(&env, &serde_json::to_string(&va::log(c)).unwrap()), None => js(&env, "[]") } }

/// 调试：B 的 known 与 A 画布对账（内容行 [y0, y1)，相对 ORIGIN 之外的绝对画布 y）
#[no_mangle]
pub extern "system" fn Java_hj_Native_vaCmp(env: JNIEnv, _c: JClass, y0: jint, y1: jint) -> jstring { match core() { Some(c) => js(&env, &va::dbg_cmp(c, y0 as i64, y1 as i64).to_string()), None => js(&env, "{}") } }
