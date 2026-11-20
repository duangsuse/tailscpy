package hj

/**
 * libhj_host.so（Rust：rd + host-net）的 JNI 入口。约定：Kotlin 单向调进 Rust，Rust 不回调 Kotlin。
 * 网络上行消息和连接事件由 `nextUp` 阻塞取（JSON）；控制类下行用 `down`（Down 的 JSON 形式）。
 */
object Native {
    @JvmStatic external fun version(): String
    /** 起 WebTransport 服务端。binds = "ip:port,…"，sans = 证书 SAN。返回 JSON：{ok, hash, token, fresh, binds} 或 {ok:false, err} */
    @JvmStatic external fun start(dir: String, binds: String, sans: String): String
    @JvmStatic external fun nextUp(timeoutMs: Int): String?
    @JvmStatic external fun down(json: String): Boolean
    @JvmStatic external fun setApps(json: String): Boolean
    /** 开会话 / 重连重发：A 的缓存作废，整屏重发 */
    @JvmStatic external fun begin(w: Int, h: Int)
    @JvmStatic external fun end()
    /** 推最新一帧（整帧 RGBA，行距 w*4）。只拷贝，编码在 Rust 线程 */
    @JvmStatic external fun frame(buf: ByteArray)
    /** A11y 图片矩形提案 [[x,y,w,h],…] */
    @JvmStatic external fun hints(json: String)
    /** A11y 的 Scroll 区域提案："[x,y,w,h]" 或 "[]" */
    @JvmStatic external fun view(y: Int)
    @JvmStatic external fun scrollRegion(json: String)
    /** 点击换算 + 点前校验：内容坐标 y → {ok, sy} / {ok:false, why} */
    @JvmStatic external fun resolveTap(y: Int, waitMs: Int): String
    @JvmStatic external fun param(key: String, v: Double): Boolean
    @JvmStatic external fun blank(): Boolean
    @JvmStatic external fun stats(): String
    /** wire 类型 JSON → postcard → JSON 往返（调试口验证用）。kind = "Up" | "Down" */
    @JvmStatic external fun roundtrip(kind: String, json: String): String
    /** 虚拟 A（实跑模式）：进程内的 A，收线上那串 Down 并用 rd 解码 */
    @JvmStatic external fun vaAttach(on: Boolean)
    @JvmStatic external fun vaState(rows: Boolean): String
    @JvmStatic external fun vaDump(path: String): String
    @JvmStatic external fun vaLog(): String
    fun load(path: String) { System.load(path) }
}
