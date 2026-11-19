package hj

/**
 * 虚拟屏上任务的 Activity 栈深与栈顶（60 §7）。shell 身份读 `dumpsys activity activities` 里该 display 那一段的 `Hist #N`；
 * 一次查询几十毫秒，所以按需调用（录制 / 注入之后），不放热路径。
 * 只反映 Activity 栈：同一 Activity 里的 Fragment / Compose 导航深度不变，这类换页靠 A11y 与像素判断。
 * 回调（任务栈监听）探针 P6 在 Probes 里对比，这里先用查询。
 */
object Stack {
    private val TASK = Regex("^\\s*\\* Task\\{")
    private val HIST = Regex("\\* Hist\\s+#\\d+: ActivityRecord\\{\\S+ u\\d+ (\\S+) t\\d+")

    /** (栈深, 栈顶 "包/类")，没有则 null。 */
    fun query(display: Int): Pair<Int, String>? =
        parse(sh("dumpsys activity activities | sed -n '/^Display #$display /,/^Display #/p'"))

    /** 虚拟屏那一段里第一个有 Hist 记录的任务：Hist 从栈顶往下排。 */
    fun parse(out: String): Pair<Int, String>? {
        var n = 0; var top = ""
        for (l in out.lineSequence()) {
            if (TASK.containsMatchIn(l)) { if (n > 0) break; continue }
            val m = HIST.find(l) ?: continue
            n++; if (top.isEmpty()) top = m.groupValues[1]
        }
        return if (n > 0) n to top else null
    }
}
