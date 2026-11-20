package hj

import android.content.Context
import android.content.ContextWrapper
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyEvent
import android.view.MotionEvent
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer

/** shell 身份冒充 com.android.shell，才能建虚拟屏、注入输入。 */
class ShellContext(base: Context) : ContextWrapper(base) {
    override fun getPackageName() = "com.android.shell"
    override fun getOpPackageName() = "com.android.shell"
    override fun getApplicationContext(): Context = this
}

fun sh(cmd: String): String = Runtime.getRuntime().exec(arrayOf("sh", "-c", cmd)).let { p ->
    val s = p.inputStream.readBytes().decodeToString() + p.errorStream.readBytes().decodeToString(); p.waitFor(); s.trim()
}

fun log(s: String) { println("[hj] $s"); System.out.flush() }

/** 一帧的元信息；seq 单调递增，t 是采集回调的单调时钟（ms）。 */
class FrameMeta(val seq: Long, val t: Long, val changedRows: Int)

/**
 * 平台层：虚拟屏 + ImageReader 采集 + 起 App + 输入注入。shell 身份，隐藏 API 走反射。
 *
 * 采集：回调里把最新一帧拷进 `cur`（rowStride 对齐处理后连续存放），并算每行 hash；
 * 与上一帧比较得出变化行数。没有新帧就什么都不做（不用定时器）。
 */
class Platform {
    private val ctx: Context
    private val dm: DisplayManager
    private val imgThread = HandlerThread("hj-img").also { it.start() }
    private var vd: VirtualDisplay? = null
    private var reader: ImageReader? = null
    @Volatile var displayId = -1; private set
    var w = 0; private set
    var h = 0; private set
    var dpi = 0; private set

    // ── 采集状态（lock 保护）──
    val lock = Object()
    private var cur = ByteArray(0)
    private var curHash = LongArray(0)
    private var tmpHash = LongArray(0)
    @Volatile var seq = 0L; private set
    @Volatile var lastFrameT = 0L; private set
    private var lastChangeT = 0L        // 最近一次“相对上一帧有行变化”的帧时间
    private val listeners = ArrayList<(FrameMeta) -> Unit>()
    /** 录制器用：每帧回调（在 lock 内、采集线程上调用，别做重活）。 */
    fun onFrame(f: ((FrameMeta) -> Unit)?, remove: ((FrameMeta) -> Unit)? = null) {
        synchronized(lock) { if (f != null) listeners.add(f); if (remove != null) listeners.remove(remove) }
    }
    fun removeFrameListener(f: (FrameMeta) -> Unit) { synchronized(lock) { listeners.remove(f) } }
    /** 帧回调拿 cur 用：在 lock 内读，不要持有引用出去。 */
    fun curLocked(): ByteArray = cur
    fun hashLocked(): LongArray = curHash

    fun context(): Context = ctx
    private val imInst: Any
    private val inject: java.lang.reflect.Method
    private val setDisp = InputEvent::class.java.getMethod("setDisplayId", Int::class.javaPrimitiveType)

    init {
        Looper.prepareMainLooper()
        val at = Class.forName("android.app.ActivityThread")
        val t = at.getMethod("systemMain").invoke(null)
        ctx = ShellContext(at.getMethod("getSystemContext").invoke(t) as Context)
        dm = DisplayManager::class.java.getDeclaredConstructor(Context::class.java).also { it.isAccessible = true }.newInstance(ctx)
        val im = Class.forName("android.hardware.input.InputManager")
        imInst = ctx.getSystemService(im) ?: error("拿不到 InputManager 服务")
        inject = im.getMethod("injectInputEvent", InputEvent::class.java, Int::class.javaPrimitiveType)
    }

    companion object {
        // scrcpy 在 Android 14+ 创建虚拟屏用的组合；不含 SHOULD_SHOW_SYSTEM_DECORATIONS，所以没有状态栏 / 导航栏
        const val PUBLIC = 1; const val PRESENTATION = 2; const val OWN_CONTENT_ONLY = 8; const val SUPPORTS_TOUCH = 1 shl 6
        const val ROTATES = 1 shl 7; const val DESTROY_ON_REMOVAL = 1 shl 8; const val TRUSTED = 1 shl 10
        const val OWN_GROUP = 1 shl 11; const val UNLOCKED = 1 shl 12; const val NO_TOUCH_FEEDBACK = 1 shl 13; const val OWN_FOCUS = 1 shl 14; const val DEVICE_GROUP = 1 shl 15
        val FLAGS = PUBLIC or PRESENTATION or OWN_CONTENT_ONLY or SUPPORTS_TOUCH or ROTATES or DESTROY_ON_REMOVAL or TRUSTED or OWN_GROUP or UNLOCKED or NO_TOUCH_FEEDBACK or OWN_FOCUS or DEVICE_GROUP
    }

    fun launcherComponent(pkg: String): String? =
        sh("cmd package resolve-activity --brief -c android.intent.category.LAUNCHER $pkg | tail -1").takeIf { it.contains('/') }

    /** 建虚拟屏（不启动 App）。失败返回原因。 */
    fun openDisplay(w: Int, h: Int, dpi: Int): String? {
        close()
        // 4 张缓冲：多留一张，回调不会因为等空缓冲而卡
        val r = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 4)
        synchronized(lock) {
            cur = ByteArray(w * h * 4); curHash = LongArray(h); tmpHash = LongArray(h); seq = 0; lastChangeT = 0; lastFrameT = 0
        }
        this.w = w; this.h = h; this.dpi = dpi
        r.setOnImageAvailableListener({ rd ->
            try {
                val img = rd.acquireLatestImage() ?: return@setOnImageAvailableListener
                try { ingest(img.planes[0].buffer, img.planes[0].rowStride, SystemClock.uptimeMillis()) } finally { img.close() }
            } catch (e: Throwable) { if (reader === rd) log("采集回调出错: $e") }
        }, Handler(imgThread.looper))
        val d = try { dm.createVirtualDisplay("hj", w, h, dpi, r.surface, FLAGS) } catch (e: Throwable) { r.close(); return "创建虚拟屏失败: ${e.cause ?: e}" }
            ?: run { r.close(); return "createVirtualDisplay 返回 null" }
        vd = d; reader = r; displayId = d.display.displayId; holdWake(true)
        return null
    }

    /** 虚拟屏的输入法策略设为 HIDE（2）：否则回落到物理屏弹键盘（Android 13+ 无系统装饰的虚拟屏默认 FALLBACK_DISPLAY）。文本用 text 注入。 */
    fun hideIme(): String {
        return try {
            val sm = Class.forName("android.os.ServiceManager").getMethod("getService", String::class.java).invoke(null, "window") as android.os.IBinder
            val wm = Class.forName("android.view.IWindowManager\$Stub").getMethod("asInterface", android.os.IBinder::class.java).invoke(null, sm)
            wm.javaClass.getMethod("setDisplayImePolicy", Int::class.javaPrimitiveType, Int::class.javaPrimitiveType).invoke(wm, displayId, 2)
            "ok"
        } catch (e: Throwable) { "失败: ${e.cause ?: e}" }
    }

    fun startApp(pkg: String): String? {
        val comp = launcherComponent(pkg) ?: return "找不到 $pkg 的启动 Activity"
        log("IME 策略 HIDE: ${hideIme()}")
        val out = sh("am start --display $displayId -f 0x10008000 -n $comp")
        log("虚拟屏 $displayId ${w}x$h@$dpi；am start $comp: ${out.lines().joinToString(" | ")}")
        return if (out.contains("Error") || out.contains("Exception")) "am start 失败: $out" else null
    }

    /** 部分唤醒锁：物理屏熄灭后设备也不休眠，虚拟屏照常出帧（探针 P7）。 */
    private var wake: android.os.PowerManager.WakeLock? = null
    fun holdWake(on: Boolean) {
        try {
            if (on && wake == null) wake = ctx.getSystemService(android.os.PowerManager::class.java).newWakeLock(android.os.PowerManager.PARTIAL_WAKE_LOCK, "hj:session").also { it.acquire() }
            if (!on) { wake?.release(); wake = null }
        } catch (e: Throwable) { log("唤醒锁失败: $e") }
    }

    fun forceStop(pkg: String) { sh("am force-stop $pkg") }

    fun close() {
        val r = reader; reader = null
        r?.setOnImageAvailableListener(null, null)
        vd?.release(); vd = null
        r?.close()
        displayId = -1; holdWake(false)
        synchronized(this) { pointers.clear() }
    }

    // ───────── 采集 ─────────
    private fun ingest(buf: ByteBuffer, rowStride: Int, tMs: Long) {
        val rowBytes = w * 4
        synchronized(lock) {
            buf.rewind()
            if (rowStride == rowBytes) buf.get(cur, 0, rowBytes * h)
            else for (y in 0 until h) { buf.position(y * rowStride); buf.get(cur, y * rowBytes, rowBytes) }
            val lb = ByteBuffer.wrap(cur).asLongBuffer()
            val per = rowBytes / 8
            var changed = 0
            for (y in 0 until h) {
                var hh = -0x61c8864680b583ebL
                val base = y * per
                for (i in 0 until per) hh = (hh xor lb.get(base + i)) * -0x4b47d1b2d7c6a5c3L + (hh ushr 29)
                tmpHash[y] = hh
                if (hh != curHash[y]) changed++
            }
            val t = tmpHash; tmpHash = curHash; curHash = t
            seq++; lastFrameT = tMs
            if (changed > 0) lastChangeT = tMs
            val m = FrameMeta(seq, tMs, changed)
            for (l in listeners) try { l(m) } catch (e: Throwable) { log("帧监听出错: $e") }
            lock.notifyAll()
        }
    }

    /** 当前帧的行 hash 快照（点按前取，作为“变化”的基准）。 */
    fun snapshotHash(): LongArray = synchronized(lock) { curHash.clone() }

    class Settle(val changed: Boolean, val settled: Boolean, val tChangeMs: Long, val tSettleMs: Long, val seq: Long, val changedRows: Int, val volatileRows: IntArray? = null)

    /**
     * 等“下一次变更阈值”再等静止：
     *   1. 相对 base 有 ≥ changeRatio×高度 行不同 → 视为发生了变化（tChange）；
     *   2. 此后 settleMs 内没有新的行变化 → 静止（tSettle）；
     *   3. 总共不超过 timeoutMs，超时返回 changed/settled 实际状态，并给出最近 1s 内一直在变的行范围（volatileRows）。
     * ignore = 要忽略的行带 [y0, y1)（走马灯文字、转圈这类持续变化的区域）：这些行既不计入变化阈值，也不阻止静止。
     * 时间用调用方线程的单调时钟，从调用时刻算起。
     */
    fun waitSettle(base: LongArray, changeRatio: Double, settleMs: Long, timeoutMs: Long, ignore: List<IntArray> = emptyList()): Settle {
        val t0 = SystemClock.uptimeMillis()
        val need = maxOf(1, Math.ceil(changeRatio * h).toInt())
        val mask = BooleanArray(h); for (r in ignore) for (y in maxOf(0, r[0]) until minOf(h, r[1])) mask[y] = true
        var tChange = -1L
        var res: Settle? = null
        var seen = base.clone()
        var lastQuiet = t0                       // 最近一次“相对上次观察有未忽略行变化”的时刻
        val recent = ArrayDeque<LongArray>()     // (时刻, ymin, ymax)
        synchronized(lock) {
            while (res == null) {
                val now = SystemClock.uptimeMillis()
                var diff = 0; var ymin = Int.MAX_VALUE; var ymax = -1
                for (y in 0 until minOf(h, base.size)) {
                    if (mask[y]) continue
                    if (curHash[y] != base[y]) diff++
                    if (curHash[y] != seen[y]) { if (y < ymin) ymin = y; ymax = y }
                }
                if (ymax >= 0) { lastQuiet = now; recent.addLast(longArrayOf(now, ymin.toLong(), ymax.toLong())); System.arraycopy(curHash, 0, seen, 0, minOf(h, seen.size)) }
                while (recent.isNotEmpty() && now - recent.first()[0] > 1000) recent.removeFirst()
                if (tChange < 0 && diff >= need) tChange = now - t0
                if (tChange >= 0 && now - maxOf(lastQuiet, t0 + tChange) >= settleMs) res = Settle(true, true, tChange, now - t0, seq, diff)
                else if (now - t0 >= timeoutMs) {
                    val vol = if (recent.isEmpty()) null else intArrayOf(maxOf(0, recent.minOf { it[1] }.toInt() - 4), minOf(h, recent.maxOf { it[2] }.toInt() + 5))   // 两侧各留 4 行：走马灯边缘有渐隐
                    res = Settle(tChange >= 0, false, tChange, now - t0, seq, diff, vol)
                } else {
                    val untilTimeout = timeoutMs - (now - t0)
                    val untilSettle = if (tChange >= 0) settleMs - (now - maxOf(lastQuiet, t0 + tChange)) else untilTimeout
                    lock.wait(maxOf(1L, minOf(untilTimeout, untilSettle)))
                }
            }
        }
        return res!!
    }

    /** 当前帧编码为 PNG（无损）或 JPEG（预览，可缩放）。返回 (bytes, seq)。 */
    fun encode(fmt: String, scale: Double, quality: Int): Pair<ByteArray, Long> {
        val copy: ByteArray; val s: Long
        synchronized(lock) { copy = cur.clone(); s = seq }
        var bmp = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        bmp.copyPixelsFromBuffer(ByteBuffer.wrap(copy))
        if (scale < 0.999) { val sc = Bitmap.createScaledBitmap(bmp, (w * scale).toInt(), (h * scale).toInt(), true); bmp.recycle(); bmp = sc }
        val out = ByteArrayOutputStream(if (fmt == "png") 1 shl 20 else 1 shl 16)
        bmp.compress(if (fmt == "png") Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, quality, out)
        bmp.recycle()
        return out.toByteArray() to s
    }

    /** 当前帧原始 RGBA 的拷贝（rd 重放 / 校验用）。 */
    fun rawCopy(): Pair<ByteArray, Long> = synchronized(lock) { cur.clone() to seq }

    // ───────── 输入 ─────────
    private class Ptr(var x: Float, var y: Float)
    private val pointers = LinkedHashMap<Int, Ptr>()
    private var downTime = 0L
    /** 注入监听：录制器记录“B 实际注入了什么”。(kind, 描述) */
    @Volatile var injectLog: ((String, String) -> Unit)? = null

    private fun send(e: InputEvent) { setDisp.invoke(e, displayId); inject.invoke(imInst, e, 0) }

    private fun motion(action: Int, now: Long, src: Int = InputDevice.SOURCE_TOUCHSCREEN) {
        val n = pointers.size
        val props = Array(n) { MotionEvent.PointerProperties() }; val coords = Array(n) { MotionEvent.PointerCoords() }
        pointers.entries.forEachIndexed { i, (id, p) -> props[i].id = id; props[i].toolType = MotionEvent.TOOL_TYPE_FINGER; coords[i].x = p.x; coords[i].y = p.y; coords[i].pressure = 1f; coords[i].size = 1f }
        val e = MotionEvent.obtain(downTime, now, action, n, props, coords, 0, 0, 1f, 1f, 0, 0, src, 0)
        try { send(e) } finally { e.recycle() }
    }

    /** phase: 0 Down 1 Move 2 Up 3 Cancel。多指按 id 聚合。所有注入串行（synchronized）。 */
    @Synchronized fun touch(id: Int, phase: Int, x: Float, y: Float) {
        if (displayId < 0) return
        val now = SystemClock.uptimeMillis()
        injectLog?.invoke("touch", "id=$id phase=$phase x=$x y=$y")
        when (phase) {
            0 -> {
                if (pointers.isEmpty()) downTime = now
                pointers[id] = Ptr(x, y)
                val idx = pointers.keys.indexOf(id)
                motion(if (pointers.size == 1) MotionEvent.ACTION_DOWN else MotionEvent.ACTION_POINTER_DOWN or (idx shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), now)
            }
            1 -> { val p = pointers[id] ?: return; p.x = x; p.y = y; motion(MotionEvent.ACTION_MOVE, now) }
            else -> {
                val p = pointers[id] ?: return; p.x = x; p.y = y
                val idx = pointers.keys.indexOf(id)
                val act = if (phase == 3) MotionEvent.ACTION_CANCEL else if (pointers.size == 1) MotionEvent.ACTION_UP else MotionEvent.ACTION_POINTER_UP or (idx shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
                motion(act, now); pointers.remove(id)
            }
        }
    }

    fun tap(x: Float, y: Float, holdMs: Long = 40) { touch(0, 0, x, y); Thread.sleep(holdMs); touch(0, 2, x, y) }

    /** 带时间戳的触摸轨迹：pts = [dtMs, x, y]，dt 相对按下时刻。严格按 dt 回放，所以惯性速度和人手一致。 */
    fun path(pts: List<Triple<Long, Float, Float>>) {
        if (pts.isEmpty()) return
        val t0 = SystemClock.uptimeMillis()
        pts.forEachIndexed { i, (dt, x, y) ->
            val wait = t0 + dt - SystemClock.uptimeMillis(); if (wait > 0) Thread.sleep(wait)
            touch(0, if (i == 0) 0 else if (i == pts.lastIndex) 2 else 1, x, y)
        }
    }

    /** 分步拖动（线性插值，每步约 stepMs）。 */
    fun drag(x1: Float, y1: Float, x2: Float, y2: Float, durMs: Long, stepMs: Long = 8) {
        val n = maxOf(2, (durMs / stepMs).toInt())
        path((0..n).map { i -> val f = i.toFloat() / n; Triple(durMs * i / n, x1 + (x2 - x1) * f, y1 + (y2 - y1) * f) })
    }

    @Synchronized fun key(code: Int, down: Boolean = true, up: Boolean = true, meta: Int = 0) {
        if (displayId < 0) return
        injectLog?.invoke("key", "code=$code")
        val now = SystemClock.uptimeMillis()
        if (down) send(KeyEvent(now, now, KeyEvent.ACTION_DOWN, code, 0, meta, -1, 0, 0, InputDevice.SOURCE_KEYBOARD))
        if (up) send(KeyEvent(now, now, KeyEvent.ACTION_UP, code, 0, meta, -1, 0, 0, InputDevice.SOURCE_KEYBOARD))
    }

    fun back() = key(KeyEvent.KEYCODE_BACK)

    /**
     * 鼠标滚轮（SOURCE_MOUSE 的 ACTION_SCROLL）。vscroll / hscroll 单位是“格”：
     * 系统用 格数 × 滚动因子（`ViewConfiguration.getScaledVerticalScrollFactor`）换算成像素，
     * 所以注入像素 N 时 vscroll = (N + 0.5) / factor（精确公式，见 30 §3；factor 由 P1 探针量出）。
     */
    @Synchronized fun wheel(x: Float, y: Float, vscroll: Float, hscroll: Float) {
        if (displayId < 0) return
        injectLog?.invoke("wheel", "x=$x y=$y v=$vscroll h=$hscroll")
        val now = SystemClock.uptimeMillis()
        val props = arrayOf(MotionEvent.PointerProperties().also { it.id = 0; it.toolType = MotionEvent.TOOL_TYPE_MOUSE })
        val c = arrayOf(MotionEvent.PointerCoords().also { it.x = x; it.y = y; it.setAxisValue(MotionEvent.AXIS_VSCROLL, vscroll); it.setAxisValue(MotionEvent.AXIS_HSCROLL, hscroll) })
        val e = MotionEvent.obtain(now, now, MotionEvent.ACTION_SCROLL, 1, props, c, 0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_MOUSE, 0)
        try { send(e) } finally { e.recycle() }
    }

    // ───────── 文本 / 剪贴板 / 视图常量 ─────────
    // 剪贴板服务按“调用包名”校验：用 ShellContext 自己构造（系统上下文取出的实例包名是 android，会被 AppOps 拒绝）
    private val clip: android.content.ClipboardManager? by lazy {
        android.content.ClipboardManager::class.java.getDeclaredConstructor(Context::class.java, Handler::class.java)
            .also { it.isAccessible = true }.newInstance(ctx, Handler(Looper.getMainLooper()))
    }
    fun clipSet(t: String) { clip?.setPrimaryClip(android.content.ClipData.newPlainText("hj", t)) }
    fun clipGet(): String? = clip?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(ctx)?.toString()

    /** 文本：能用虚拟键盘字符映射的逐键注入；映射不了的（中文等）整段“设剪贴板 + 粘贴”。返回走了哪条路。 */
    fun text(s: String): String {
        val kcm = android.view.KeyCharacterMap.load(android.view.KeyCharacterMap.VIRTUAL_KEYBOARD)
        val evs = kcm.getEvents(s.toCharArray())
        if (evs != null && s.all { it.code < 128 }) {
            for (e in evs) { send(e) }
            return "keys"
        }
        clipSet(s); key(KeyEvent.KEYCODE_PASTE)
        return "paste"
    }

    /** ViewConfiguration 的滚动常量：滚轮 1 格 = scrollFactor 像素（精确公式 vscroll=(N+0.5)/factor 用它）。 */
    fun viewConfig(): Map<String, Any> {
        val vc = android.view.ViewConfiguration.get(ctx)
        return mapOf("vScrollFactor" to vc.scaledVerticalScrollFactor, "hScrollFactor" to vc.scaledHorizontalScrollFactor,
            "touchSlop" to vc.scaledTouchSlop, "minFling" to vc.scaledMinimumFlingVelocity, "maxFling" to vc.scaledMaximumFlingVelocity, "density" to ctx.resources.displayMetrics.density)
    }
}
