package hj

import org.json.JSONObject
import java.io.BufferedWriter
import java.io.DataOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.OutputStreamWriter
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.zip.Deflater

/**
 * 语料录制器（80 §3）：只记录 App 的行为——输入 → 画面 → A11y，不经过编码器和调度器。
 * 就是 host 本身（`rec_start` / `--record <目录>`），和生产同一条采集、A11y、注入路径。
 *
 *   <dir>/manifest.json   设备、几何、包名、设置、schema
 *   <dir>/events.jsonl    一行一个事件，t = B 单调时钟（ms，相对录制开始）
 *   <dir>/frames.bin      记录序列：关键帧（整帧）每 KEYFRAME_EVERY 帧一个，其余只存变化的行段；每条 deflate 压缩，无损
 *   <dir>/frames.idx      每帧 16 字节：seq u32, t u32(ms), offset u64（均 LE）
 *   <dir>/trees/NNNN.json 全量节点树
 *
 * frames.bin 记录：seq u32, kind u8（0 整帧 / 1 增量）, rawLen u32, zLen u32, z[zLen]；
 *   整帧 raw = w*h*4 的 RGBA；增量 raw = 若干 [y0 u16, n u16, n 行字节]。
 */
class Recorder(private val dir: File, private val pf: Platform, private val a11y: A11y, private val manifest: JSONObject) {
    companion object { const val KEYFRAME_EVERY = 120; const val SCHEMA = 1 }

    private val t0 = android.os.SystemClock.uptimeMillis()
    private val events: BufferedWriter
    private val framesBin: DataOutputStream
    private val framesIdx: DataOutputStream
    private var binOff = 0L
    private var prevHash: LongArray? = null
    private var sinceKey = 0
    private var treeN = 0
    @Volatile var stackTop = ""
    private val writer = Executors.newSingleThreadExecutor { Thread(it, "hj-rec").also { t -> t.isDaemon = true } }
    private val sched = Executors.newSingleThreadScheduledExecutor { Thread(it, "hj-rec-tree").also { t -> t.isDaemon = true } }
    private var pendingTree: ScheduledFuture<*>? = null
    @Volatile private var closed = false
    private val frameL: (FrameMeta) -> Unit = { m -> onFrame(m) }
    private val a11yL: (JSONObject) -> Unit = { e -> onA11yEvent(e) }

    init {
        dir.mkdirs(); File(dir, "trees").mkdirs()
        manifest.put("schema", SCHEMA).put("w", pf.w).put("h", pf.h).put("dpi", pf.dpi)
        File(dir, "manifest.json").writeText(manifest.toString(2))
        events = BufferedWriter(OutputStreamWriter(FileOutputStream(File(dir, "events.jsonl")), Charsets.UTF_8))
        framesBin = DataOutputStream(FileOutputStream(File(dir, "frames.bin")).buffered(1 shl 16))
        framesIdx = DataOutputStream(FileOutputStream(File(dir, "frames.idx")).buffered(1 shl 12))
        pf.injectLog = { kind, d -> event("inject", JSONObject().put("kind", kind).put("d", d)) }
        pf.onFrame(frameL)
        a11y.events.add(a11yL)
        event("mark", JSONObject().put("name", "rec_start"))
    }

    private fun now() = android.os.SystemClock.uptimeMillis() - t0

    @Synchronized fun event(type: String, o: JSONObject = JSONObject()) {
        if (closed) return
        o.put("t", now()).put("type", type)
        events.write(o.toString()); events.write("\n"); events.flush()
    }

    fun step(o: JSONObject) = event("step", o)
    fun mark(name: String) = event("mark", JSONObject().put("name", name))

    /** 在 Platform.lock 内、采集线程上调用：只拷变化的行，压缩和落盘交给写线程。 */
    private fun onFrame(m: FrameMeta) {
        if (closed) return
        val h = pf.hashLocked(); val cur = pf.curLocked(); val rb = pf.w * 4
        val prev = prevHash
        val key = prev == null || sinceKey >= KEYFRAME_EVERY
        val raw: ByteArray
        if (key) { raw = cur.clone(); sinceKey = 0 }
        else {
            val bo = java.io.ByteArrayOutputStream(); val d = DataOutputStream(bo)
            var y = 0
            while (y < pf.h) {
                if (h[y] == prev!![y]) { y++; continue }
                var e = y; while (e < pf.h && h[e] != prev[e]) e++
                d.writeShort(y); d.writeShort(e - y)   // BE，读取端按同样顺序解
                d.write(cur, y * rb, (e - y) * rb)
                y = e
            }
            raw = bo.toByteArray()
        }
        prevHash = h.clone(); sinceKey++
        if (!key && raw.isEmpty()) return   // 内容相同的重复帧
        val seq = m.seq; val t = m.t - t0; val stack = stackTop
        writer.execute {
            try {
                val z = ByteArray(raw.size + raw.size / 100 + 64)
                val df = Deflater(1); df.setInput(raw); df.finish(); val zl = df.deflate(z); df.end()
                val bb = ByteBuffer.allocate(13).order(ByteOrder.LITTLE_ENDIAN)
                bb.putInt(seq.toInt()).put(if (key) 0 else 1).putInt(raw.size).putInt(zl)
                val off = binOff
                framesBin.write(bb.array()); framesBin.write(z, 0, zl)
                binOff += 13 + zl
                val ib = ByteBuffer.allocate(16).order(ByteOrder.LITTLE_ENDIAN).putInt(seq.toInt()).putInt(t.toInt()).putLong(off)
                framesIdx.write(ib.array())
            } catch (e: Throwable) { log("录帧出错: $e") }
        }
        event("frame", JSONObject().put("seq", seq).put("ft", t).put("pkg", stack.substringBefore('/')).put("key", key).put("rows", m.changedRows))
    }

    private fun onA11yEvent(e: JSONObject) {
        if (closed) return
        event("a11y_event", e)
        val k = e.optString("kind")
        // 窗口状态 / 内容变化 / 滚动：去抖 250ms 后取一棵全量树
        if (k.contains("WINDOW_STATE") || k.contains("WINDOW_CONTENT") || k.contains("VIEW_SCROLLED") || k.contains("WINDOWS_CHANGED"))
            synchronized(this) {
                pendingTree?.cancel(false)
                pendingTree = sched.schedule({ try { dumpTree() } catch (x: Throwable) { log("录树出错: $x") } }, 250, TimeUnit.MILLISECONDS)
            }
    }

    /** 取一棵全量树并记下（绑当时的采集帧序号）。 */
    fun dumpTree(): JSONObject? {
        if (closed || pf.displayId < 0 || !a11y.connected) return null
        val seq = pf.seq
        val tr = a11y.dump(pf.displayId)
        tr.put("seq", seq)
        val n = synchronized(this) { treeN++ }
        val name = "trees/%04d.json".format(n)
        File(dir, name).writeText(tr.toString())
        event("a11y_tree", JSONObject().put("file", name).put("seq", seq).put("ipcMs", tr.optDouble("ipcMs")))
        return tr
    }

    fun noteStack(depth: Int, top: String) {
        stackTop = top
        event("stack", JSONObject().put("depth", depth).put("top", top))
    }

    fun close() {
        if (closed) return
        mark("rec_stop")
        pf.injectLog = null; pf.removeFrameListener(frameL); a11y.events.remove(a11yL)
        pendingTree?.cancel(false)
        writer.shutdown(); writer.awaitTermination(10, TimeUnit.SECONDS)
        synchronized(this) { closed = true; events.close(); framesBin.close(); framesIdx.close() }
        sched.shutdownNow()
    }
}
