# T06：Android preview verified cache hit（2026-09-29）

状态：`in_progress`。PR #173 已 squash 合并为 `0c2771e`。本切片把已有 preview output lock
和 verified artifact manifest 语义接入 Android default-debug live preview；不把 release、custom
signing 或 signing-sensitive 输出标为可复用缓存。

## 行为

- matrix planner 将 Android ABI-specific BuildKey、target-specific output root、cache policy 和
  default `~/.android/debug.keystore` 内容 hash通过受控环境传入 preview；preview 在同一 output
  root 上取得 `BuildOutputLock`，锁覆盖 manifest lookup、miss 后的 cargo-ndk/Gradle 构建和
  manifest 发布；
- 只有 cache policy 允许且 default debug keystore 当前 hash仍与规划值一致时才尝试命中。release、
  custom signing、敏感 Android 配置、local `build.rs`、SDK/NDK/cargo-ndk/JDK 身份不可读或
  keystore 缺失等情况只走正常构建；keystore 在 lookup 或构建期间变化会拒绝继续复用/发布；
- manifest 必须绑定 Android platform、当前 BuildKey 和 ABI，逐文件验证完整 JNI staging 目录与
  Gradle debug APK 输出目录的路径、大小和内容 hash；APK metadata 还必须解析到唯一存在的 APK；
- 命中时直接返回已验证 APK，跳过 `rustup target add`、cargo-ndk 和 Gradle；live loop 随后仍
  按本次 run 安装、配置和启动 Android app，不复用设备安装或进程身份；
- manifest 缺失、platform/key 不匹配、JNI/APK 根不匹配、文件新增/删除/变化或 metadata 无效
  都按 cache miss 回到正常构建。命中之前仍需完成已有的输入扫描和 BuildKey 计算。

## 验证

- 新增单测覆盖 Android preview manifest 的 JNI/APK 根匹配、错误根拒绝和发布后完整文件验证；
- 本 PR 本地通过 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked
  -- -D warnings`、`cargo test --workspace --locked`（338 个单元测试及全部集成/协议测试）、
  `cargo x check-design-docs` 和 `git diff --check`；
- PR #173 的三 OS check、desktop-template、android-template、baseline-driver required checks
  全部通过；macOS capture helper deadline 初次波动后重跑通过。没有发布或 tag；验证不冒充完整
  GPUI Android app、emulator/device 安装或连续场景验收。

## 未覆盖

- Android release/custom/复杂 signing 仍需完整建模签名输入；Gradle plugin、wrapper distribution、
  NDK/build-script 隐藏 I/O 和相同 SDK revision 下的 package 内容变化仍未完全纳入 BuildKey；
- iOS physical cache hit、跨独立命令构建 ownership、同 key 在途任务 coalescing/取消引用、增量
  索引、预热、性能预算和真实 Android emulator/device 连续矩阵仍未接入。
