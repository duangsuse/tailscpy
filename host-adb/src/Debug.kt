package hj

import android.util.Base64
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.io.PrintWriter
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket

/** 测试只允许这些包（用户限定，60 §4）。动态白名单 = 本表 ∩ 本机已安装。 */
val WHITELIST = listOf(
    "com.android.settings", "com.absinthe.libchecker", "com.sirma.mobile.bible.android", "de.szalkowski.activitylauncher",
    "com.azefsw.audioconnect", "com.android.calendar", "com.miui.calculator", "InfinityLoop1309.NewPipeEnhanced",
    "org.qpython.qpy", "com.and96.aggregator_news", "com.aefyr.sai",
)

/**
 * 调试口（60 §10）：只绑回环，一行一条 JSON 请求 → 一行一条 JSON 响应，经 `adb forward` 访问。**不做鉴权，绝不能绑到回环以外。**
 * 每个连接一个线程；所有注入在 Platform 里串行。直接驱动类命令在此；“扮演 A”的 wire 命令在 M1 加。
 */
class Debug(private val port: Int, private val pf: Platform, private val a11y: A11y, private val settings: Settings,
            private val autoRecord: File?) {
    var pkg: String? = null; private set
    var session: Session? = null
    private var rec: Recorder? = null
    private val dev = "${android.os.Build.MANUFACTURER} ${android.os.Build.MODEL}"
    @Volatile private var quit = false
    val quitting get() = quit

    fun serve() {
        val ss = ServerSocket(port, 8, InetAddress.getLoopbackAddress())
        log("调试口 127.0.0.1:$port pid=${android.os.Process.myPid()} sdk=${android.os.Build.VERSION.SDK_INT} $dev")
        while (!quit) {
            val s = try { ss.accept() } catch (e: Throwable) { break }
            Thread({ conn(s) }, "hj-conn").also { it.isDaemon = true }.start()
        }
    }

    private fun conn(s: Socket) {
        s.use {
            val r = BufferedReader(InputStreamReader(s.getInputStream(), Charsets.UTF_8))
            val w = PrintWriter(s.getOutputStream().bufferedWriter(Charsets.UTF_8), false)
            while (true) {
                val line = r.readLine() ?: break
                if (line.isBlank()) continue
                val res = try { handle(JSONObject(line)) } catch (e: Throwable) {
                    log("命令出错: $line → $e"); JSONObject().put("ok", false).put("err", (e.cause ?: e).toString())
                }
                w.println(res.toString()); w.flush()
                if (quit) break
            }
        }
    }

    private fun ok() = JSONObject().put("ok", true)
    private fun err(m: String) = JSONObject().put("ok", false).put("err", m)

    private fun settleOpts(r: JSONObject): JSONObject? =
        if (r.has("settle") && !r.isNull("settle")) (r.optJSONObject("settle") ?: JSONObject()) else null

    @Volatile private var baseHash: LongArray? = null

    /** settle 之后的收尾：按 settle 选项等“变化阈值 + 静止”，再按 stack / shot 选项附上栈与截图。 */
    private fun finish(r: JSONObject, base: LongArray?, res: JSONObject): JSONObject {
        val so = settleOpts(r)
        if (so != null && base != null) {
            val ig = so.optJSONArray("ignore")?.let { a -> (0 until a.length()).map { i -> a.getJSONArray(i).let { intArrayOf(it.getInt(0), it.getInt(1)) } } } ?: emptyList()
            val s = pf.waitSettle(base, so.optDouble("change", 0.005), so.optLong("settle_ms", 300), so.optLong("timeout_ms", 3000), ig)
            res.put("settle", JSONObject().put("changed", s.changed).put("settled", s.settled).put("tChange", s.tChangeMs)
                .put("tSettle", s.tSettleMs).put("seq", s.seq).put("rows", s.changedRows).also { o -> s.volatileRows?.let { o.put("volatile", JSONArray().put(it[0]).put(it[1])) } })
        }
        if (r.optBoolean("stack")) Stack.query(pf.displayId)?.let { (d, top) -> res.put("stack", JSONObject().put("depth", d).put("top", top)); rec?.noteStack(d, top) }
        if (r.optBoolean("shot")) { val (b, seq) = pf.encode("png", 1.0, 100); res.put("shot", Base64.encodeToString(b, Base64.NO_WRAP)).put("shotSeq", seq) }
        return res
    }

    /** 注入类命令的公共外壳：取基准 → 注入 → finish。 */
    private fun act(r: JSONObject, name: String, body: () -> Unit): JSONObject {
        if (pf.displayId < 0) return err("没有会话（先 open）")
        val base = if (settleOpts(r) != null) pf.snapshotHash() else null
        rec?.step(JSONObject().put("act", name).put("req", r))
        body()
        return finish(r, base, ok().put("seq0", pf.seq))
    }

    private fun handle(r: JSONObject): JSONObject {
        when (val cmd = r.getString("cmd")) {
            "ping" -> return ok().put("rd", try { Native.version() } catch (e: Throwable) { null }).put("pid", android.os.Process.myPid()).put("sdk", android.os.Build.VERSION.SDK_INT).put("dev", dev)
            "status" -> return ok().put("display", pf.displayId).put("pkg", pkg).put("w", pf.w).put("h", pf.h).put("dpi", pf.dpi)
                .put("seq", pf.seq).put("a11y", a11y.connected).put("settings", settings.applied).put("rec", rec != null)
            "apps" -> {
                val inst = sh("pm list packages").lineSequence().map { it.removePrefix("package:").trim() }.toSet()
                return ok().put("apps", JSONArray(WHITELIST.filter { it in inst }))
            }
            "open" -> return openSession(r)
            "close" -> { closeSession(); return ok() }
            "quit" -> { closeSession(); quit = true; return ok() }
            "frame" -> {
                val since = r.optLong("since", -1)
                // since 只在同一块虚拟屏内有效：重开会话后帧序号从头计，客户端带上次见到的 display 来判断
                if (r.optInt("disp", -2) == pf.displayId && pf.seq <= since) return ok().put("same", true).put("seq", pf.seq).put("display", pf.displayId)
                if (pf.seq == 0L) return err("还没有帧")
                val fmt = r.optString("fmt", "png")
                val (b, seq) = pf.encode(fmt, r.optDouble("scale", 1.0), r.optInt("q", if (fmt == "png") 100 else 70))
                return ok().put("seq", seq).put("display", pf.displayId).put("fmt", fmt).put("w", pf.w).put("h", pf.h).put("data", Base64.encodeToString(b, Base64.NO_WRAP))
            }
            "frame_file" -> {   // 取一帧写到 B 上的文件（PNG），再由调用方 adb pull；大图不走调试口
                val (b, seq) = pf.encode("png", 1.0, 100)
                File(r.getString("path")).writeBytes(b)
                return ok().put("seq", seq).put("bytes", b.size)
            }
            "base" -> { baseHash = pf.snapshotHash(); return ok().put("seq", pf.seq) }   // 记下“现在”作为变化基准（拖动实时注入时用）
            "settle" -> {   // 等变化 + 静止；from_base=true 时以最近一次 base 为基准，否则以现在为基准
                val so = settleOpts(r) ?: JSONObject().also { r.put("settle", it) }
                val base = if (r.optBoolean("from_base")) baseHash else pf.snapshotHash()
                return finish(r, base ?: pf.snapshotHash(), ok())
            }
            "tree" -> {
                if (pf.displayId < 0) return err("没有会话")
                val seq = pf.seq
                val t = rec?.dumpTree() ?: a11y.dump(pf.displayId, r.optBoolean("full", true)).put("seq", seq)
                return if (r.optBoolean("brief")) ok().put("seq", seq).put("ipcMs", t.optDouble("ipcMs")).put("text", a11y.brief(t))
                else ok().put("tree", t)
            }
            "stack" -> {
                val s = Stack.query(pf.displayId) ?: return ok().put("depth", 0).put("top", "")
                rec?.noteStack(s.first, s.second)
                return ok().put("depth", s.first).put("top", s.second)
            }
            "tap" -> return act(r, cmd) { pf.tap(r.getDouble("x").toFloat(), r.getDouble("y").toFloat()) }
            "long" -> return act(r, cmd) { pf.tap(r.getDouble("x").toFloat(), r.getDouble("y").toFloat(), r.optLong("ms", 700)) }
            "swipe" -> return act(r, cmd) {
                pf.drag(r.getDouble("x1").toFloat(), r.getDouble("y1").toFloat(), r.getDouble("x2").toFloat(), r.getDouble("y2").toFloat(), r.optLong("ms", 300))
            }
            "path" -> return act(r, cmd) {   // pts: [[dt, x, y], ...]
                val a = r.getJSONArray("pts")
                pf.path((0 until a.length()).map { val p = a.getJSONArray(it); Triple(p.getLong(0), p.getDouble(1).toFloat(), p.getDouble(2).toFloat()) })
            }
            "touch" -> return act(r, cmd) { pf.touch(r.optInt("id", 0), r.getInt("phase"), r.getDouble("x").toFloat(), r.getDouble("y").toFloat()) }
            "wheel" -> return act(r, cmd) { pf.wheel(r.getDouble("x").toFloat(), r.getDouble("y").toFloat(), r.optDouble("v", 0.0).toFloat(), r.optDouble("h", 0.0).toFloat()) }
            "back" -> return act(r, cmd) { pf.back() }
            "key" -> return act(r, cmd) { pf.key(r.getInt("code")) }
            "text" -> return act(r, cmd) { r.put("via", pf.text(r.getString("s"))) }.put("via", r.optString("via"))
            "clipset" -> { pf.clipSet(r.getString("s")); return ok() }
            "clipget" -> return ok().put("s", pf.clipGet())
            "viewcfg" -> return ok().also { o -> pf.viewConfig().forEach { (k, v) -> o.put(k, v) } }
            "scroll_to" -> return act(r, cmd) { r.put("msg", a11y.scrollTo(pf.displayId, r.getInt("row"), r.optString("id").takeIf { it.isNotEmpty() })) }.put("msg", r.optString("msg"))
            "jpeg_bench" -> {   // 当前帧 JPEG 编码耗时（B 端 ms/MP）；n 次取中位数
                val n = r.optInt("n", 7); val q = r.optInt("q", 70); val ts = ArrayList<Double>(); var sz = 0
                repeat(n) { val t = System.nanoTime(); sz = pf.encode("jpeg", 1.0, q).first.size; ts.add((System.nanoTime() - t) / 1e6) }
                ts.sort(); val mp = pf.w * pf.h / 1e6
                return ok().put("medianMs", ts[ts.size / 2]).put("msPerMP", ts[ts.size / 2] / mp).put("bytes", sz).put("q", q)
            }
            // ── 扮演 A：wire 类型的 JSON 形式，和网络上来的 Up 走同一条处理路径（60 §10）──
            "wire" -> { val s = session ?: return err("网络未启动（host 要带 --so）"); s.handle(JSONTokener(r.get("msg").let { if (it is String) JSONObject.quote(it) else it.toString() }).nextValue()); return ok() }
            "wire_log" -> return ok().put("down", (session ?: return err("网络未启动")).recentDown(r.optInt("n", 20)))
            "wire_rt" -> return ok().put("rt", JSONObject(Native.roundtrip(r.getString("kind"), r.get("json").let { if (it is String) JSONObject.quote(it) else it.toString() })))
            "net" -> return ok().put("net", session?.info ?: JSONObject()).put("allow", JSONArray((session?.allow ?: emptySet<String>()).toList())).put("active", session?.active ?: false)
            "net_stats" -> return ok().put("stats", JSONObject(Native.stats()))
            "va_attach" -> { Native.vaAttach(r.optBoolean("on", true)); return ok() }
            "va_cmp" -> return ok().put("cmp", JSONObject(Native.vaCmp(r.optInt("y0", 1048576), r.optInt("y1", 1048576 + 3000))))
            "va_log" -> return ok().put("log", JSONArray(Native.vaLog()))
            "va_dump" -> return ok().put("r", Native.vaDump(r.getString("path")))
            "va_state" -> return ok().put("va", JSONObject(Native.vaState(r.optBoolean("rows"))))
            "param" -> return ok().put("accepted", Native.param(r.getString("key"), r.getDouble("val")))
            "policy" -> { val s = session ?: return err("网络未启动"); s.allow.clear(); r.optJSONArray("allow")?.let { a -> for (i in 0 until a.length()) s.allow.add(a.getString(i)) }; return ok().put("allow", JSONArray(s.allow.toList())) }
            "mark" -> { rec?.mark(r.getString("name")); return ok() }
            "step" -> { rec?.step(r.getJSONObject("data")); return ok() }
            "rec_start" -> { startRecord(File(r.getString("dir"))); return ok() }
            "rec_stop" -> { rec?.close(); rec = null; return ok() }
            else -> return err("未知命令 $cmd")
        }
    }

    /** 打开会话：改设置 → 建虚拟屏 → 连 A11y → 冷启动 App（60 §1）。调试口 `open` 和网络 `Up::Open` 共用。 */
    fun openSession(r: JSONObject): JSONObject {        val p = r.getString("pkg")
        if (p !in WHITELIST) return err("不在白名单: $p")
        if (pf.displayId >= 0) closeSession()
        val phys = sh("wm size").substringAfterLast(' ').split('x')
        val dens = sh("wm density").substringAfterLast(' ').toIntOrNull() ?: 420
        val w = r.optInt("w", phys[0].toInt()); val h = r.optInt("h", phys[1].toInt()); val dpi = r.optInt("dpi", dens)
        settings.apply(r.optString("animator", "0"))
        pf.openDisplay(w, h, dpi)?.let { settings.restore(); return err(it) }
        try { a11y.connect() } catch (e: Throwable) { log("UiAutomation 连接失败（继续，无 A11y）: ${e.cause ?: e}") }
        pf.forceStop(p)   // 冷启动：每次 open 都从进程不存在开始，录制与回放起点一致
        pf.startApp(p)?.let { closeSession(); return err(it) }
        pkg = p
        if (autoRecord != null && rec == null) startRecord(File(autoRecord, p))
        // 等首帧
        val t0 = System.currentTimeMillis()
        synchronized(pf.lock) { while (pf.seq == 0L && System.currentTimeMillis() - t0 < 5000) pf.lock.wait(100) }
        return ok().put("display", pf.displayId).put("w", w).put("h", h).put("dpi", dpi).put("a11y", a11y.connected).put("seq", pf.seq)
    }

    private fun startRecord(dir: File) {
        rec?.close()
        val man = JSONObject().put("device", dev).put("sdk", android.os.Build.VERSION.SDK_INT).put("pkg", pkg)
            .put("pkgVersion", sh("dumpsys package $pkg 2>/dev/null | grep versionName | head -1").substringAfter('=', "?"))
            .put("settings", JSONObject().also { o -> for (k in Settings.KEYS) o.put(k, sh("settings get global $k")) })
            .put("hostVersion", "m0")
        rec = Recorder(dir, pf, a11y, man)
        Stack.query(pf.displayId)?.let { (d, top) -> rec?.noteStack(d, top) }   // 种下栈顶，让开头的帧也带包名
        log("录制 → $dir")
    }

    fun closeSession() {
        session?.onClosed()
        rec?.close(); rec = null
        pkg?.let { pf.forceStop(it) }
        pf.close(); a11y.disconnect(); settings.restore(); pkg = null
    }
}
