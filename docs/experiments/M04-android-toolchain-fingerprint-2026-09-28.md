# M04：Android SDK/NDK toolchain fingerprint 切片（2026-09-28）

状态：in_progress。本切片将可观测的 Android SDK/NDK/JDK/cargo-ndk 身份绑定到非 live
Android BuildKey；无法确认构建实际使用的工具链时禁用 cache hit，但仍允许正常构建。

## 行为

- 选择 `ANDROID_HOME`/`ANDROID_SDK_ROOT` 时要求所有已设置路径指向同一目录；枚举其中
  `platforms` 与 `build-tools` 的 `source.properties` revision，并排序后形成无路径清单；
- NDK 必须能从 `ANDROID_NDK_HOME/source.properties` 读取 `Pkg.Revision`；若设置的
  `NDK_HOME` 与它不一致，身份视为不明确；
- 将 `cargo ndk --version` 与 Gradle wrapper launcher 选择的 Java（优先 `JAVA_HOME/bin/java`）版本输出摘要
  和 SDK/NDK revision 一起哈希，再与
  `rustc -vV` 摘要组成 toolchain fingerprint；BuildKey 只保留摘要，不保留工具安装路径；
- SDK/NDK 环境冲突、包 revision 不可读或版本探测失败时写入 unavailable 标记并禁用缓存
  查找；debug keystore 与 custom/release signing 的既有 bypass 规则继续生效；
- `ANDROID_SDK_ROOT` 和 `NDK_HOME` 也进入 relevant environment allowlist。

## 验证

- 单元测试验证 SDK platform/build-tools revision 改变会改变身份、不完整或 symlink package
  拒绝生成指纹、不同 SDK 根冲突会被拒绝，以及缺少工具链身份时缓存明确 bypass；
- Android template CI 安装 cargo-ndk 4.1.2 后，在 SDK 34/build-tools 34/NDK 27.2.12479018
  与 Java 21 环境将 fingerprint 测试设为 required；该 job 只证明工具身份可读，不执行 GPUI
  原生 APK 构建或设备运行；
- 本机版本探测为 cargo-ndk 4.1.2、Amazon Corretto OpenJDK 21.0.8、NDK 27.2.12479018；
  SDK package metadata 含 android-34/35/36 与 build-tools 30.0.3/34.0.0/35.0.0/35.0.1/36.0.0。
  这是身份探测证据，不是 Android APK 构建/安装/启动证据；
- PR 门槛运行 workspace fmt、clippy、测试和设计文档检查；Android 模板 CI 不冒充真实
  emulator/device build、install 或启动证据。

## 未覆盖

- 只读取 Android SDK package revision，不逐文件校验 `android.jar`、build-tools 二进制或 NDK
  内容；包管理器若在相同 revision 下替换文件，当前指纹不能发现；
- Gradle wrapper URL/脚本受冻结源码输入保护，但下载到用户 Gradle cache 的 distribution、
  AGP/plugin 任意隐藏 I/O、Gradle daemon 与 toolchain 自动下载仍未完整建模；
- emulator/device 安装运行证据、custom/release signing cache、同 key 在途任务共享、取消
  引用与缓存容量清理仍未接入。
