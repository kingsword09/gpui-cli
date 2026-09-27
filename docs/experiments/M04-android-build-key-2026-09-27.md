# M04：Android BuildKey 输出接入切片（2026-09-27）

状态：`in_progress`。本切片把非 live 的 `gpui build/run android` 接到 ABI 集合对应的
BuildKey，并隔离 Cargo target、cargo-ndk JNI staging 和 Gradle app build 输出；不宣称
live Android、真实设备矩阵或冻结快照构建已经接入。

## 行为

- Android BuildKey 将规范化后的 ABI 集合和对应 Rust target 集合作为显式维度；ABI 输入
  顺序变化不会改变 key，profile、源码/native manifest、toolchain、环境 allowlist 或 ABI
  集合变化会改变 key；
- `cargo-ndk` 使用 `.gpui/builds/android/<key>/cargo-target`，并将 JNI 输出写入
  `.gpui/builds/android/<key>/native-staging/android/jni-libs/<abi-set>`；ABI 子目录仍由
  cargo-ndk 保留，多个 ABI 不再写入项目的 `mobile/android/gradle/app/src/main/jniLibs`；
- Gradle 通过 `gpui.jniLibsDir` 读取 key 私有 JNI 根目录，通过 `gpui.buildDir` 将 APK
  中间产物放入 `.gpui/builds/android/<key>/gradle-build`；生成项目未收到 CLI 参数时仍
  使用原来的项目内默认目录，便于手工 Gradle 构建；
- Android 环境 allowlist 纳入 `ANDROID_HOME`、`ANDROID_NDK_HOME` 和 `JAVA_HOME`，但
  未尝试自动发现 NDK/Gradle 隐藏 I/O。

## 验证

- `runner::build_inputs` 测试验证多 ABI key 的顺序稳定性、Android JNI staging 和 Gradle
  build 路径；输出布局测试验证 Android layout 创建这些目录且 iOS/desktop 不携带该路径；
- 生成 Android 模板测试验证 `gpui.jniLibsDir` 和 `gpui.buildDir` 参数接线；本 PR 门槛
  结果：`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo test --workspace --locked`（222 个 unit tests 及全部 integration/doc tests）和
  `cargo x check-design-docs` 均通过；设计文档检查报告 82 个 Markdown 文件、113 个
  本地链接、35 个任务和 68 个验收用例。CI Android/desktop 模板检查随后作为 PR 门槛；
- 当前没有把本机 Android emulator/device 启动结果写成通过证据，缺少设备时保持
  not_run/unavailable。

## 未覆盖

- `gpui live android` 仍使用 live builder 的项目内输出；
- snapshot build orchestration、缓存命中、同 key 在途任务合并和构建期间输入锁定尚未
  接入；
- build.rs、Gradle、NDK 的未声明隐藏输入和真实多设备/ABI 运行矩阵仍需后续平台切片。
