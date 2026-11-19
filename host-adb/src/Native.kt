package hj

/** libhj_host.so（Rust：rd + host-net）的 JNI 入口。M0 只有版本函数；采集零拷贝直读与 WebTransport 在 M1 接入。 */
object Native {
    @JvmStatic external fun version(): String
    fun load(path: String) { System.load(path) }
}
