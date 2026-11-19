## 1. 启动：构建、起服务、拿 URL

```sh
export B=BQ89AQAAJZHENJIV
# Make 构建（WASM + NDK .so + Kotlin dex）并推到 B

./e2e/build.sh
# 起守护进程 + adb forward，末行打印带令牌的 URL（令牌也在 /tmp/hj-token.<端口>）
./e2e/run.sh 8002            # 可选：<fps=30> <zstd级别=3>
./e2e/run.sh 8002 stop       # 停（run.sh 的 stop 只认守护进程，会话里的 App 随会话关）
```

浏览器（PC）打开：

```
http://localhost:8002/?t=<令牌>&app=com.android.settings&scr=1080x2400@60,420dp
```

A 手机看： 只要两个设备在同一个局域网，服务端也提供类miniserve+viewer，可供连接到其他地址

```
#TODO 如何在A上打开
```

## 2. 对比表格（活镜 vs scrcpy，手工比较）

scrcpy 无法镜像我们的虚拟屏，所以是**各自从 App 刚启动开始**、同一套拖动序列各跑一遍，只比较量级：

```sh
export B=..

e2e/cmp.py --auto modes --scrcpy 8011 "$url"

# 常驻一个 scrcpy relay 页面
cmp-scp/relay.py -s $B --port 8010 --size 1080x2400/420 --app com.android.settings

# 控制服务端的方法（格式需与wire保持一致）
adb -s $B shell 'echo my_cmd >> /data/local/tmp/hj-ctl.8002'
```
