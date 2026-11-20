package hj

import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener
import java.io.File
import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * 会话层（60 §1、§4）：网络上来的 `Up`（JSON 形式）→ 输入注入 / 开关会话；B 的状态变化 → `Down`（JSON 形式）下发。
 * 调试口的 `wire` 命令走同一个 `handle`，所以脚本扮演 A 和真浏览器是同一条处理路径（60 §10）。
 *
 * 白名单策略（`allow`，host 配置，A 不能改）：
 *   空            单应用严格模式：栈顶只允许 `Open.pkg`；越界 → 注入 BACK + `Fault{OffSession}`；任务消失 → `Fault{AppGone}` 并结束会话
 *   额外包名      多包白名单：这些包之间可切换，不能退到桌面
 *   `[popup]`     允许任何包弹出的临时窗口（权限框、分享面板…），不允许桌面
 *   `*`           任意切换，也允许退回桌面（桌面起的新 App 仍在虚拟屏上，因为桌面本身就在虚拟屏上）
 */
class Session(private val dbg: Debug, private val pf: Platform, private val a11y: A11y, private val settings: Settings) {
    val allow = LinkedHashSet<String>()
    @Volatile var active = false; private set
    private var sessPkg: String? = null
    private var geomW = 0; private var geomH = 0; private var geomDpi = 0
    @Volatile private var connected = false
    @Volatile private var detachAt = 0L
    @Volatile private var detachKeepS = 60L
    private val downLog = ArrayDeque<String>()
    private var frameL: ((FrameMeta) -> Unit)? = null
    private var a11yL: ((JSONObject) -> Unit)? = null
    @Volatile private var viewUrl = ""
    @Volatile var info: JSONObject = JSONObject()

    // watch 线程用
    @Volatile private var hintAt = 0L          // >0：到点取一次树（图片矩形 + 可编辑焦点）
    @Volatile private var stackAt = 0L         // >0：到点查一次栈（策略检查）
    private var lastEditable = false
    private var lastHints = ""; private var lastRegion = ""
    private var offStrikes = 0; private var goneStrikes = 0; private var blankStrikes = 0
    private var lastStackCheck = 0L; private var lastTree = 0L
    private val homeComp: String? by lazy { sh("cmd package resolve-activity --brief -a android.intent.action.MAIN -c android.intent.category.HOME | tail -1").trim().takeIf { it.contains('/') } }
    private val home: String? get() = homeComp?.substringBefore('/')

    // ───────── 启动 ─────────
    /** 起网络：绑回环 + tailnet（100.64/10）网卡；`lan` 额外绑私网网卡（开发用，默认不开：50 §5）。 */
    fun start(dir: File, netPort: Int, lan: Boolean, extraBinds: List<String>, viewer: String, keepS: Long) {
        detachKeepS = keepS
        val ips = LinkedHashMap<String, String>()   // ip → 类型
        ips["127.0.0.1"] = "loopback"
        for (ni in NetworkInterface.getNetworkInterfaces()) for (a in ni.inetAddresses) {
            if (a !is Inet4Address || a.isLoopbackAddress) continue
            val b = a.address.map { it.toInt() and 255 }
            val tail = b[0] == 100 && b[1] in 64..127
            val priv = a.isSiteLocalAddress
            if (tail) ips[a.hostAddress!!] = "tailnet" else if (priv && lan) ips[a.hostAddress!!] = "lan"
        }
        for (e in extraBinds) ips[e] = "extra"
        val binds = ips.keys.joinToString(",") { "$it:$netPort" }
        val res = JSONObject(Native.start(dir.path, binds, ips.keys.joinToString(",") + ",localhost"))
        if (!res.optBoolean("ok")) { log("网络启动失败: ${res.optString("err")}"); return }
        Native.setApps(appsJson())
        val frag = "h=${res.getString("hash")}&t=${res.getString("token")}"
        val urls = JSONArray(); val sb = StringBuilder()
        for ((ip, kind) in ips) { val u = "$viewer#b=$ip:$netPort&$frag"; urls.put(JSONObject().put("kind", kind).put("url", u)); sb.append("  [$kind] $u\n") }
        viewUrl = viewer
        info = JSONObject().put("port", netPort).put("hash", res.getString("hash")).put("token", res.getString("token")).put("fresh", res.optBoolean("fresh")).put("urls", urls)
        log("WebTransport UDP :$netPort${if (res.optBoolean("fresh")) "（新证书，URL 已变）" else ""}\n查看页 URL：\n$sb")
        File(dir, "url.txt").writeText(urls.let { a -> (0 until a.length()).joinToString("\n") { i -> a.getJSONObject(i).let { "${it.getString("kind")} ${it.getString("url")}" } } } + "\n")
        Thread({ upLoop() }, "hj-up").also { it.isDaemon = true }.start()
        Thread({ watchLoop() }, "hj-watch").also { it.isDaemon = true }.start()
    }

    private fun appsJson(): String {
        val inst = sh("pm list packages").lineSequence().map { it.removePrefix("package:").trim() }.toSet()
        val pm = try { pf.context().packageManager } catch (e: Throwable) { null }
        val arr = JSONArray()
        for (p in WHITELIST.filter { it in inst }) {
            val name = try { pm?.getApplicationLabel(pm.getApplicationInfo(p, 0))?.toString() } catch (e: Throwable) { null } ?: p
            arr.put(JSONObject().put("pkg", p).put("name", name).put("icon", JSONObject.NULL))
        }
        return arr.toString()
    }

    // ───────── 下行 ─────────
    private fun down(o: JSONObject) {
        val s = o.toString()
        synchronized(downLog) { downLog.addLast(s); while (downLog.size > 200) downLog.removeFirst() }
        try { Native.down(s) } catch (e: Throwable) { log("下发失败: $e") }
    }
    fun fault(code: String, msg: String) { log("Fault $code: $msg"); down(JSONObject().put("Fault", JSONObject().put("code", code).put("msg", msg))) }
    fun recentDown(n: Int): JSONArray = synchronized(downLog) { JSONArray(downLog.toList().takeLast(n).map { JSONObject(it) }) }

    // ───────── 上行 ─────────
    private fun upLoop() {
        while (true) {
            val line = try { Native.nextUp(500) } catch (e: Throwable) { log("nextUp: $e"); Thread.sleep(500); null } ?: continue
            try { event(JSONObject(line)) } catch (e: Throwable) { log("处理上行出错: $line → ${e.cause ?: e}") }
        }
    }

    private fun event(e: JSONObject) {
        when (e.getString("ev")) {
            "scroll" -> need { pf.wheel(e.getInt("x").toFloat(), e.getInt("y").toFloat(), -e.getDouble("notches").toFloat(), 0f) }   // 采集头调度要的一次滚动（正 = 往下滚；AXIS_VSCROLL 为负）
            "connected" -> {
                connected = true; detachAt = 0
                log("A 已连接 ${e.optString("peer")}")
                if (active) resend()   // 重连到还活着的会话：A 的缓存作废，重发（50 §3 / M1 固定 Reset）
            }
            "disconnected" -> {
                connected = false
                if (active) { detachAt = System.currentTimeMillis() + detachKeepS * 1000; log("A 断开，保留会话 ${detachKeepS}s") }
            }
            "conn_error" -> log("连接出错: ${e.optString("msg")}")
            "up" -> handle(JSONTokener(e.get("msg").let { if (it is String) JSONObject.quote(it) else it.toString() }).nextValue())
        }
    }

    /** Up 的 JSON 形式（单元变体是字符串，其余是 {名字: 字段}）。网络和调试口 `wire` 共用。 */
    fun handle(v: Any?) {
        val name: String; val b: JSONObject
        when (v) { is String -> { name = v; b = JSONObject() } ; is JSONObject -> { name = v.keys().next(); b = v.getJSONObject(name) } ; else -> return }
        when (name) {
            "Open" -> openApp(b.getString("pkg"), b.optJSONObject("geom"))
            "Close" -> closeApp()
            "Tap" -> need { tapAt(b, 40) }
            "LongPress" -> need { tapAt(b, 650) }
            "Touch" -> need { val (x, y) = pt(b); pf.touch(0, b.getInt("phase"), x, y); if (b.getInt("phase") == 2) afterInput() }
            // Wheel 的 dx/dy 单位是“格”（A 把像素除以一格像素数）。浏览器 deltaY>0 = 往下滚 = Android 的 AXIS_VSCROLL 为负
            "Wheel" -> need { val (x, y) = pt(b); pf.wheel(x, y, -b.getDouble("dy").toFloat(), b.getDouble("dx").toFloat()); afterInput() }
            "Back" -> need { pf.back(); afterInput() }
            "Key" -> need {
                val code = b.getInt("code"); val meta = b.optInt("meta"); val dn = b.getBoolean("down")
                pf.key(code, down = dn, up = !dn, meta = meta); afterInput()
                if (dn && (meta and 0x1000) != 0 && (code == 31 || code == 52)) scheduleClip()   // Ctrl+C / Ctrl+X：稍后把 B 的剪贴板回给 A
            }
            "Text" -> need { pf.text(b.getString("s")); afterInput() }
            "ClipSet" -> pf.clipSet(b.getString("s"))
            "ClipGet" -> sendClip()
            "Resync" -> if (active) resend()
            "View" -> Native.view(b.getInt("y"))   // 经调试口 wire 的 View；网络上来的 View 在 Rust 里直接处理
            "Forget", "Persisted", "Inventory", "VerifyFail" -> {}   // M2/M3 才有用
            else -> log("忽略 Up::$name")
        }
    }
    /** 点按 / 长按。Scroll 区域（region 1）里的点是内容坐标：先换算成屏幕坐标并做点前校验，A 看到的和 B 现在显示的不一致就不点（Stale）。 */
    private fun tapAt(b: JSONObject, hold: Long) {
        val at = b.getJSONObject("at"); val x = at.getDouble("x").toFloat(); var y = at.getDouble("y").toFloat()
        if (at.optInt("region") == 1) {
            val r = JSONObject(Native.resolveTap(at.getInt("y"), 600))
            if (!r.optBoolean("ok")) { fault("Stale", "内容已变化或还没就位（${r.optString("why")}），请再点一次"); return }
            y = r.getInt("sy").toFloat()
        }
        pf.tap(x, y, hold); afterInput()
    }
    private fun need(f: () -> Unit) { if (active) f() }
    private fun pt(b: JSONObject): Pair<Float, Float> = b.getJSONObject("at").let { it.getDouble("x").toFloat() to it.getDouble("y").toFloat() }

    private fun sendClip() { pf.clipGet()?.let { down(JSONObject().put("Clip", JSONObject().put("s", it))) } }
    private fun scheduleClip() { Thread({ Thread.sleep(250); sendClip() }, "hj-clip").also { it.isDaemon = true }.start() }

    private fun afterInput() { val n = System.currentTimeMillis(); stackAt = n + 400; hintAt = n + 300 }

    // ───────── 会话 ─────────
    fun openApp(pkg: String, geom: JSONObject?) {
        if (pkg !in WHITELIST) { fault("OffSession", "不在白名单: $pkg"); return }
        teardown()
        val req = JSONObject().put("pkg", pkg)
        geom?.let { req.put("w", it.getInt("w")).put("h", it.getInt("h")).put("dpi", it.getInt("dpi")) }
        val r = dbg.openSession(req)
        if (!r.optBoolean("ok")) { fault("AppGone", r.optString("err")); return }
        sessPkg = pkg; geomW = r.getInt("w"); geomH = r.getInt("h"); geomDpi = r.getInt("dpi")
        offStrikes = 0; goneStrikes = 0; blankStrikes = 0; lastEditable = false; lastHints = ""; lastRegion = ""
        val fl: (FrameMeta) -> Unit = { m -> if (m.changedRows > 0) Native.frame(pf.curLocked()) }
        frameL = fl; pf.onFrame(fl)
        val al: (JSONObject) -> Unit = { ev -> val k = ev.optString("kind"); if (k.contains("WINDOW") || k.contains("VIEW_FOCUSED") || k.contains("SCROLLED") || k.contains("TEXT_SELECTION")) hintAt = System.currentTimeMillis() + 120 }
        a11yL = al; a11y.events.add(al)
        active = true
        resend()
        hintAt = System.currentTimeMillis() + 400; stackAt = System.currentTimeMillis() + 1500
    }

    /** 新会话 / 重连：Opened → begin（A 的缓存作废）→ 把当前帧推进去 */
    private fun resend() {
        down(JSONObject().put("Opened", JSONObject().put("pkg", sessPkg).put("geom", JSONObject().put("w", geomW).put("h", geomH).put("dpi", geomDpi))))
        Native.begin(geomW, geomH)
        synchronized(pf.lock) { if (pf.seq > 0) Native.frame(pf.curLocked()) }
        lastHints = ""; lastRegion = ""; lastEditable = false; hintAt = System.currentTimeMillis() + 100
    }

    /** A 要求关闭会话（回到选包界面） */
    fun closeApp() { teardown(); dbg.closeSession() }

    private fun teardown() {
        active = false
        frameL?.let { pf.removeFrameListener(it) }; frameL = null
        a11yL?.let { a11y.events.remove(it) }; a11yL = null
        try { Native.end() } catch (_: Throwable) {}
    }

    /** Debug.closeSession 经过这里（含会话因故结束）。 */
    fun onClosed() { if (active || frameL != null) teardown() }

    private fun endWith(code: String, msg: String) { fault(code, msg); closeApp() }

    // ───────── 监视线程：A11y 提案、策略、BlankFrame、断线超时 ─────────
    private fun allowedTop(top: String): Boolean {
        if ("*" in allow) return true
        val isHome = top == home
        if (top == sessPkg || top in allow) return !isHome
        if ("[popup]" in allow && !isHome) return true
        return false
    }

    private fun watchLoop() {
        var lastSec = 0L
        while (true) {
            Thread.sleep(50)
            val now = System.currentTimeMillis()
            if (!active) continue
            try {
                if (detachAt > 0 && now >= detachAt) { detachAt = 0; log("断线超时，关闭会话"); closeApp(); continue }
                if (hintAt in 1..now && now - lastTree >= 150) { hintAt = 0; lastTree = now; refreshHints() }
                if ((stackAt in 1..now) || now - lastStackCheck >= 2000) { stackAt = 0; lastStackCheck = now; checkStack() }
                if (now - lastSec >= 1000) {
                    lastSec = now
                    if (Native.blank()) { if (++blankStrikes == 3) fault("BlankFrame", "B 端画面为空（受保护内容或 App 未启动）") } else blankStrikes = 0
                }
            } catch (e: Throwable) { log("监视线程出错: ${e.cause ?: e}") }
        }
    }

    private fun refreshHints() {
        if (!a11y.connected || pf.displayId < 0) return
        val h = a11y.imageRects(pf.displayId, 3600).toString()
        if (h != lastHints) { lastHints = h; Native.hints(h) }
        val rg = a11y.scrollRegion(pf.displayId).toString()
        if (rg != lastRegion) { lastRegion = rg; Native.scrollRegion(rg) }
        val ed = a11y.editableFocus(pf.displayId)
        if (ed != lastEditable) { lastEditable = ed; down(JSONObject().put("Focus", JSONObject().put("editable", ed))) }
    }

    private fun checkStack() {
        val s = Stack.query(pf.displayId)
        if (s == null) {
            if (++goneStrikes >= 2) {
                if ("*" in allow && launchHome()) { goneStrikes = 0; return }
                endWith("AppGone", "应用已退出或回到桌面")
            }
            return
        }
        goneStrikes = 0
        val top = s.first.let { s.second.substringBefore('/') }
        if (allowedTop(top)) { offStrikes = 0; return }
        pf.back()
        if (++offStrikes >= 4) { endWith("AppGone", "反复越界（$top），会话结束"); return }
        fault("OffSession", "该操作超出会话允许的应用范围（$top），已返回")
        stackAt = System.currentTimeMillis() + 500
    }

    /** `*` 模式：任务没了就把桌面起在虚拟屏上，之后从桌面点开的 App 自然在虚拟屏上。 */
    private fun launchHome(): Boolean {
        // 只给 HOME intent 会被“送达物理屏上已在运行的桌面”；显式组件名才会起在虚拟屏上（真机实测，见 probe-*.md）
        val out = sh("am start --display ${pf.displayId} -n ${homeComp ?: return false} -f 0x10000000")
        log("`*`：桌面起在虚拟屏: ${out.lines().joinToString(" | ")}")
        return !(out.contains("Error") || out.contains("Exception"))
    }
}
