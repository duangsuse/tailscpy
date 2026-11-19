package hj

import org.json.JSONObject
import java.io.File

/**
 * 会话期间的全局设置（60 §2）。只动这四项，别的一概不碰。
 * 原值在修改之前写到 restore.json；还原发生在四处：正常退出、崩溃处理器、独立看门狗子进程、下次启动兜底。
 * 看门狗是一个 `sh` 循环，每 0.5s `kill -0 <host_pid>`；host 被 kill -9 之后 1–2 秒内读 restore.json 还原。
 */
class Settings(private val dir: File = File("/data/local/tmp/hj")) {
    companion object {
        /** key → 会话中的值。animator_duration_scale 默认 0，可调（`animator` 参数）。 */
        val KEYS = listOf("window_animation_scale", "transition_animation_scale", "animator_duration_scale", "disable_window_blurs")
    }
    private val restoreFile = File(dir, "restore.json")
    private val watchdogFile = File(dir, "watchdog.sh")
    @Volatile var applied = false; private set

    init { dir.mkdirs() }

    private fun get(k: String) = sh("settings get global $k").let { if (it == "null" || it.isEmpty()) null else it }
    private fun put(k: String, v: String?) { if (v == null) sh("settings delete global $k") else sh("settings put global $k $v") }

    /** 启动第 1 步：有遗留就先还原（上次是被杀掉、看门狗也没来得及）。 */
    fun restoreLeftover() {
        if (restoreFile.exists()) { log("发现上次遗留的 restore.json，先还原"); restore() }
    }

    fun apply(animator: String = "0") {
        if (applied) return
        // 先写盘再改：restore.json 里 JSON null 表示原来没设置过该项（还原时 delete）
        val orig = JSONObject()
        for (k in KEYS) orig.put(k, get(k) ?: JSONObject.NULL)
        restoreFile.writeText(orig.toString())
        startWatchdog()
        put("window_animation_scale", "0"); put("transition_animation_scale", "0")
        put("animator_duration_scale", animator); put("disable_window_blurs", "1")
        applied = true
        log("会话设置已应用（原值 $orig）")
    }

    fun setAnimator(v: String) { if (applied) put("animator_duration_scale", v) }

    /** 还原并删除 restore.json。幂等。 */
    fun restore() {
        if (restoreFile.exists()) {
            try {
                val o = JSONObject(restoreFile.readText())
                for (k in KEYS) put(k, if (o.isNull(k)) null else o.getString(k))
                log("设置已还原 $o")
            } catch (e: Throwable) { log("还原出错: $e") }
            restoreFile.delete()
        }
        applied = false
    }

    private fun startWatchdog() {
        // restore.json 是扁平 JSON：{"k":"v","k2":null}。sh 里用 sed 拆成 k=v 行；null → delete。
        watchdogFile.writeText("""#!/system/bin/sh
PID=${'$'}1; F=${restoreFile.path}
while kill -0 ${'$'}PID 2>/dev/null; do sleep 0.5; done
[ -f ${'$'}F ] || exit 0
(sed 's/[{}"]//g; s/,/\n/g' ${'$'}F; echo) | while IFS=: read k v; do
  [ -z "${'$'}k" ] && continue
  if [ "${'$'}v" = null ]; then settings delete global ${'$'}k; else settings put global ${'$'}k ${'$'}v; fi
done
rm -f ${'$'}F
""")
        val pid = android.os.Process.myPid()
        // setsid + nohup：脱离 host 的会话 / 进程组，host 被杀时它不跟着死
        Runtime.getRuntime().exec(arrayOf("sh", "-c", "setsid nohup sh ${watchdogFile.path} $pid >/dev/null 2>&1 &"))
    }
}
