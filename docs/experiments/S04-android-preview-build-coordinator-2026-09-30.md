# S04：Android default-debug preview BuildKey coordinator（2026-09-30）

状态：PR #183 已 squash 合并为 `d949322`。本切片把 Android default-debug live preview 构建接入
BuildKey preview coordinator；release/custom-signing、缺少默认 debug keystore 指纹、cache-disabled
或不可复用 BuildKey 的路径仍走原有 output lock/构建流程。本记录不扩展为真实 Android 设备连续验收。

## 实现范围

- default-debug preview 对同一 Android BuildKey 共享一个 preview attempt；leader 在同一 output lock
  内执行 cargo-ndk 与 Gradle，并发布绑定 Android/BuildKey/ABI 的 preview manifest。
- follower 等待 attempt 终态，并重新验证 preview manifest 的逐文件 size/hash、platform/BuildKey
  绑定、JNI staging 根和 Gradle debug APK 输出根；APK metadata 仍必须能解析到单一 APK。
- coordinator verifier 同时复核当前默认 debug keystore 指纹仍等于规划时的 BuildKey 输入，避免在
  keystore 变化后接受旧 attempt。
- cargo-ndk 非零结果共享为 `BuildFailed`，构建期间输入变化共享为 `Superseded`；release/custom
  signing 和其他未建模输入不进入 coordinator。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（351 个单元测试及全部集成/协议测试）
- `git diff --check`

新增回归验证 JNI/APK 输出根匹配、输出文件篡改和错误 APK 根均被拒绝。PR #183 的两组 required CI
中 Linux/macOS/Windows check、desktop-template、android-template 和 baseline-driver 全部通过。

## 未覆盖与下一步

三端 preview coordinator 的基本 leader/follower 路径已完成，但调用者取消引用、heartbeat/fencing、
queued/cancelled/partial 状态、隐藏构建输入发现和真实设备连续矩阵验收仍未实现。下一步进入这些
coordinator 语义的收口，先补失败/取消/订阅者生命周期证据，再考虑真实三端体验验证。未发布版本，未创建 tag。
