# T06：Android default-debug BuildKey manifest cache-hit 切片（2026-09-28）

状态：in_progress。本切片仅为非 live、默认 debug signing 的 Android build/run 启用本地
manifest cache hit；release/custom signing、live Android 和真实任务共享不在范围内。

## 行为

- BuildKey native hash 纳入本机默认 ~/.android/debug.keystore 的 SHA-256 内容摘要，不写出
  keystore 路径或内容；构建命中和发布前都会复核该文件未发生变化；
- 发现 release variant、本地敏感 Android 配置、自定义 signing DSL，或默认 debug keystore
  缺失/不是普通文件时，不读取缓存，按正常路径重建；缺失 keystore 的首次 Gradle build
  可能生成该文件，后续调用因新指纹得到新 BuildKey，需再构建一次后才可稳定命中；
- local.properties/keystore.properties 内容不会进入 FrozenBuildRoot；Android SDK/NDK 需由
  ANDROID_HOME/ANDROID_NDK_HOME 提供，依赖 keystore.properties 的自定义 release signing
  当前不可用，详见
  [M04 frozen sensitive input filter](M04-frozen-sensitive-input-filter-2026-09-28.md)；
- Android 请求按 BuildKey 获取 OS 文件锁，锁覆盖 manifest lookup、miss 后的 cargo-ndk/
  Gradle 构建和 manifest 发布；
- Android BuildKey 纳入 SDK 已安装 platform/build-tools revision、`ANDROID_NDK_HOME` 的 NDK
  revision、`cargo ndk --version` 与 Gradle launcher Java 版本摘要；未能唯一确认 SDK/NDK 路径或读取
  工具版本时 cache hit bypass，细节见
  [M04 Android toolchain fingerprint](M04-android-toolchain-fingerprint-2026-09-28.md)；
- 命中要求 read_verified 校验 Android platform、BuildKey 和全部已登记文件，并要求 roots
  精确等于当前 JNI staging 目录和 APK variant 输出目录；APK metadata 还须能解析出唯一、
  存在的 APK；
- debug 命中跳过 cargo-ndk 与 Gradle；gpui run android 仍为本次调用选择设备、安装并启动，
  不复用安装或进程身份；命中前仍需冻结输入并计算 BuildKey。

## 验证

- 单元测试覆盖默认 debug keystore 指纹变化、构建期 keystore 变化拒绝继续发布、release/
  sensitive/custom signing/缺失 keystore bypass，以及 Android manifest roots 与 APK metadata
  的完整命中/拒绝路径；
- 本 PR 运行 workspace fmt、clippy、全量测试和设计文档检查；CI Android template fixture
  不冒充真实 NDK/emulator/device 构建或安装证据；
- 等锁当前不可取消；OS 锁串行化请求，不表示共享一个可订阅/取消的在途任务。

## 未覆盖

- Gradle 脚本的 signing 检测基于显式配置标记，无法静态证明任意第三方插件没有隐藏签名输入；
- SDK/NDK package 内容与相同 revision 的文件替换、Gradle wrapper 下载 distribution、AGP/
  Gradle plugin 隐藏读取和任意 build-script I/O 未完整纳入 BuildKey，不能宣称跨环境完全
  可复现；release signing 和复杂 keystore 来源也仍未建模；
- Android custom/release cache hit、iOS 真机 cache hit、live builder、在途任务取消引用、
  缓存清理与容量预算仍未接入。
