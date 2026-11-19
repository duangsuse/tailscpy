package hj

import android.accessibilityservice.AccessibilityServiceInfo
import android.graphics.Rect
import android.os.HandlerThread
import android.os.Looper
import android.util.SparseArray
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.CopyOnWriteArrayList

/**
 * UiAutomation 连接（60 §5）：shell 身份取节点树，带 FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES，
 * 所以 B 上的读屏等无障碍服务不会被停掉。UiAutomation 同一时刻只能有一个持有者：host 运行时别跑 uiautomator dump / Appium。
 */
class A11y {
    private var ua: Any? = null
    private val thread = HandlerThread("hj-a11y").also { it.start() }
    val events = CopyOnWriteArrayList<(JSONObject) -> Unit>()
    @Volatile var connected = false; private set

    fun connect() {
        if (connected) return
        val conn = Class.forName("android.app.UiAutomationConnection").getConstructor().newInstance()
        val u = Class.forName("android.app.UiAutomation").getConstructor(Looper::class.java, Class.forName("android.app.IUiAutomationConnection"))
            .newInstance(thread.looper, conn)
        val c = u.javaClass
        c.getMethod("connect", Int::class.javaPrimitiveType).invoke(u, 1 /* FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES */)
        val info = c.getMethod("getServiceInfo").invoke(u) as AccessibilityServiceInfo
        info.flags = info.flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS or
            AccessibilityServiceInfo.FLAG_INCLUDE_NOT_IMPORTANT_VIEWS or AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS
        info.eventTypes = AccessibilityEvent.TYPES_ALL_MASK
        c.getMethod("setServiceInfo", AccessibilityServiceInfo::class.java).invoke(u, info)
        val lsn = Class.forName("android.app.UiAutomation\$OnAccessibilityEventListener")
        val proxy = java.lang.reflect.Proxy.newProxyInstance(lsn.classLoader, arrayOf(lsn)) { _, m, a ->
            if (m.name == "onAccessibilityEvent") try { dispatch(a[0] as AccessibilityEvent) } catch (e: Throwable) { log("A11y 事件出错: $e") }
            null
        }
        c.getMethod("setOnAccessibilityEventListener", lsn).invoke(u, proxy)
        ua = u; connected = true
    }

    fun disconnect() {
        val u = ua ?: return
        try { u.javaClass.getMethod("disconnect").invoke(u) } catch (e: Throwable) { log("A11y 断开出错: $e") }
        ua = null; connected = false
    }

    private fun dispatch(e: AccessibilityEvent) {
        if (events.isEmpty()) return
        val o = JSONObject().put("kind", AccessibilityEvent.eventTypeToString(e.eventType)).put("pkg", e.packageName?.toString())
            .put("cls", e.className?.toString()).put("t", android.os.SystemClock.uptimeMillis())
            .put("scrollY", e.scrollY).put("maxScrollY", e.maxScrollY).put("scrollX", e.scrollX).put("maxScrollX", e.maxScrollX)
            .put("fromIndex", e.fromIndex).put("toIndex", e.toIndex).put("itemCount", e.itemCount)
            .put("windowChange", e.windowChanges).put("contentChange", e.contentChangeTypes)
        e.source?.let { o.put("src", JSONObject().put("id", it.viewIdResourceName).put("cls", it.className?.toString())) }
        for (l in events) l(o)
    }

    @Suppress("UNCHECKED_CAST")
    private fun windowsOn(display: Int): List<AccessibilityWindowInfo> {
        val u = ua ?: error("A11y 未连接")
        val all = u.javaClass.getMethod("getWindowsOnAllDisplays").invoke(u) as SparseArray<List<AccessibilityWindowInfo>>
        return all.get(display) ?: emptyList()
    }

    /** 虚拟屏上全部窗口的全量节点树。bound/文本/CollectionInfo/CollectionItemInfo(rowIndex) 等，供录制与选择器用。 */
    fun dump(display: Int, full: Boolean = true): JSONObject {
        val t0 = System.nanoTime()
        val arr = JSONArray()
        for (w in windowsOn(display)) {
            val o = JSONObject().put("id", w.id).put("layer", w.layer).put("type", w.type).put("title", w.title?.toString())
                .put("active", w.isActive).put("focused", w.isFocused)
            val r = Rect(); w.getBoundsInScreen(r); o.put("bounds", rect(r))
            w.root?.let { o.put("root", node(it, full, 0)) }
            arr.put(o)
        }
        return JSONObject().put("display", display).put("windows", arr).put("ipcMs", (System.nanoTime() - t0) / 1e6)
    }

    private fun rect(r: Rect) = JSONArray().put(r.left).put(r.top).put(r.right).put(r.bottom)

    private fun node(n: AccessibilityNodeInfo, full: Boolean, depth: Int): JSONObject {
        val o = JSONObject()
        o.put("cls", n.className?.toString()).put("id", n.viewIdResourceName)
        val r = Rect(); n.getBoundsInScreen(r); o.put("b", rect(r))
        if (n.text != null) o.put("text", n.text.toString())
        if (n.contentDescription != null) o.put("desc", n.contentDescription.toString())
        if (n.isClickable) o.put("click", true)
        if (n.isLongClickable) o.put("long", true)
        if (n.isScrollable) o.put("scroll", true)
        if (n.isEditable) o.put("edit", true)
        if (n.isFocused) o.put("focus", true)
        if (n.isChecked) o.put("checked", true)
        if (!n.isVisibleToUser) o.put("hidden", true)
        if (!n.isEnabled) o.put("disabled", true)
        if (full) {
            n.collectionInfo?.let { o.put("coll", JSONObject().put("rows", it.rowCount).put("cols", it.columnCount).put("hier", it.isHierarchical)) }
            n.collectionItemInfo?.let { o.put("item", JSONObject().put("row", it.rowIndex).put("col", it.columnIndex).put("rowSpan", it.rowSpan).put("colSpan", it.columnSpan)) }
            n.rangeInfo?.let { o.put("range", JSONArray().put(it.min.toDouble()).put(it.current.toDouble()).put(it.max.toDouble())) }
            // 可滚方向：从动作表读（A11y 只给 scrollable 标志；前后向可滚由 SCROLL_FORWARD / BACKWARD 动作暴露）
            val acts = n.actionList.map { it.id }
            val dirs = StringBuilder()
            if (AccessibilityNodeInfo.ACTION_SCROLL_FORWARD in acts) dirs.append('F')
            if (AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD in acts) dirs.append('B')
            if (dirs.isNotEmpty()) o.put("scrollDirs", dirs.toString())
            n.paneTitle?.let { o.put("pane", it.toString()) }
        }
        val kids = JSONArray()
        if (depth < 60) for (i in 0 until n.childCount) n.getChild(i)?.let { kids.put(node(it, full, depth + 1)) }
        if (kids.length() > 0) o.put("kids", kids)
        return o
    }

    /** 节点树摘要（每行一个有意义的节点），调试口打印用。 */
    fun brief(tree: JSONObject): String {
        val sb = StringBuilder()
        val ws = tree.getJSONArray("windows")
        for (i in 0 until ws.length()) {
            val w = ws.getJSONObject(i)
            sb.append("window ${w.opt("id")} title=${w.opt("title")} type=${w.opt("type")} bounds=${w.opt("bounds")}\n")
            w.optJSONObject("root")?.let { walk(it, 1, sb) }
        }
        return sb.toString()
    }

    private fun walk(n: JSONObject, d: Int, sb: StringBuilder) {
        val keep = n.has("id") || n.has("text") || n.has("desc") || n.has("scroll") || n.has("coll") || n.has("item")
        var nd = d
        if (keep) {
            nd = d + 1
            sb.append("  ".repeat(d)).append(n.optString("cls").substringAfterLast('.'))
            n.optString("id").takeIf { it.isNotEmpty() }?.let { sb.append(" #").append(it.substringAfter(":id/")) }
            n.optString("text").takeIf { it.isNotEmpty() }?.let { sb.append(" \"").append(it.take(24)).append('"') }
            n.optString("desc").takeIf { it.isNotEmpty() }?.let { sb.append(" ~").append(it.take(24)) }
            sb.append(" ").append(n.opt("b"))
            if (n.has("scroll")) sb.append(" scroll=").append(n.optString("scrollDirs"))
            n.optJSONObject("coll")?.let { sb.append(" rows=").append(it.opt("rows")) }
            n.optJSONObject("item")?.let { sb.append(" rowIndex=").append(it.opt("row")) }
            sb.append('\n')
        }
        n.optJSONArray("kids")?.let { for (i in 0 until it.length()) walk(it.getJSONObject(i), nd, sb) }
    }

    /** 在虚拟屏上找最大的可滚动节点（或按 resource-id 精确找），对它执行动作。返回描述。 */
    fun scrollTo(display: Int, rowIndex: Int, resId: String? = null): String {
        var best: AccessibilityNodeInfo? = null; var bestArea = -1
        fun walk(n: AccessibilityNodeInfo, d: Int) {
            if (n.isScrollable && (resId == null || n.viewIdResourceName == resId)) {
                val r = Rect(); n.getBoundsInScreen(r); val a = r.width() * r.height()
                if (a > bestArea) { bestArea = a; best = n }
            }
            if (d < 40) for (i in 0 until n.childCount) n.getChild(i)?.let { walk(it, d + 1) }
        }
        for (w in windowsOn(display)) w.root?.let { walk(it, 0) }
        val n = best ?: return "没有可滚动节点"
        val b = android.os.Bundle().also { it.putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_ROW_INT, rowIndex); it.putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_COLUMN_INT, 0) }
        val ok = n.performAction(android.R.id.accessibilityActionScrollToPosition, b)
        return "${n.viewIdResourceName ?: n.className} → row $rowIndex: $ok"
    }
}
