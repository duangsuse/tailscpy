package hj

import java.io.File

// 用法（在 B 上，经 adb shell 的 app_process 启动）：
//   app_process -Djava.class.path=/data/local/tmp/hj/hj.dex /system/bin hj.MainKt [--port 7701] [--record <目录>]
// 启动顺序（60 §1）：1 读遗留设置并还原  2 装未捕获异常处理器  3 起调试口  4 等 open（设置 / 虚拟屏 / A11y / App 都在 open 里按序做）

fun main(args: Array<String>) {
    var port = 7701; var record: File? = null; var so: String? = null
    var netPort = 4433; var lan = false; var allow = ""; var viewer = "http://localhost:8080/"; var keepS = 60L; var noNet = false
    var i = 0
    while (i < args.size) {
        when (args[i]) {
            "--port" -> port = args[++i].toInt(); "--record" -> record = File(args[++i]); "--so" -> so = args[++i]
            "--net-port" -> netPort = args[++i].toInt(); "--lan" -> lan = true; "--allow" -> allow = args[++i]; "--viewer" -> viewer = args[++i]
            "--detach-keep" -> keepS = args[++i].toLong(); "--no-net" -> noNet = true
        }
        i++
    }
    so?.let { try { Native.load(it) } catch (e: Throwable) { log("加载 $it 失败: $e") } }
    val settings = Settings()
    settings.restoreLeftover()   // 1
    // 2：Android 默认处理器遇到任何线程未捕获异常都会 kill 整个进程；这里记日志并还原设置，其余线程继续
    Thread.setDefaultUncaughtExceptionHandler { t, e ->
        log("线程 ${t.name} 未捕获异常: $e"); e.printStackTrace(System.out); System.out.flush()
        try { settings.restore() } catch (_: Throwable) {}
    }
    val pf = Platform()
    val a11y = A11y()
    val dbg = Debug(port, pf, a11y, settings, record)
    Runtime.getRuntime().addShutdownHook(Thread { try { dbg.closeSession() } catch (_: Throwable) {} })
    // 3：网络（有 .so 才起）。启动顺序里“起网络、打印 URL”在“等 Open”之前（60 §1）
    var loaded = false
    so?.let { try { Native.version(); loaded = true } catch (e: Throwable) { log("libhj_host 不可用: $e") } }
    if (loaded && !noNet) {
        val sess = Session(dbg, pf, a11y, settings); dbg.session = sess
        // `[popup]` / `*` 过 adb shell 会被当成 glob：命令行里写 popup / any
        allow.split(',').filter { it.isNotBlank() }.map { it.trim() }.map { when (it) { "popup" -> "[popup]"; "any" -> "*"; else -> it } }.forEach { sess.allow.add(it) }
        sess.start(File("/data/local/tmp/hj"), netPort, lan, emptyList(), viewer, keepS)
    }
    dbg.serve()
    dbg.closeSession()
    Runtime.getRuntime().halt(0)
}
