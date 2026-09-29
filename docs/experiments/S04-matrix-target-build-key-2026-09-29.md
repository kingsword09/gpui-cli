# S04：matrix target-specific BuildKey 证据（2026-09-29）

状态：`in_progress`。PR #161 已 squash 合并为 `ae9ae94`。本切片在 #157 的共享 frozen
workspace root 和 #159 的 per-cell context 基础上，计算每个 ready target 的 target-specific
BuildKey，并把 key hash 写入该 target 的每个 cell context。

## 实现范围

- desktop/macOS/Linux/Windows matrix target 使用 desktop BuildKey 维度。
- iOS matrix target 使用当前 simulator runner 对应的 `aarch64-apple-ios-sim` key 维度。
- Android matrix target 使用显式 ABI 的 Android BuildKey 维度；不同 ABI 不共用 key。
- 所有 key 都从同一 shared frozen root 计算；只对 admission 后仍 ready 的 target 计算，
  unavailable cell 不会被强行赋予构建证据。
- key hash 进入 per-cell `CheckContext.build_key`，与 shared snapshot hash 一起进入
  `MatrixReport`；本切片不改变 cell 状态聚合。

## 验证

PR #161 合并前已通过：

- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（331 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增测试验证 BuildKey hash 从 frozen matrix root 计算并满足安全的 SHA-256 路径形状。PR #161
的三 OS check、两组 desktop-template、两组 android-template 和 baseline-driver required CI
全部通过。该 PR 未发布版本或 tag。

## 未覆盖与边界

- 这里只提供 target-specific key evidence；Cargo target、Gradle build、Xcode DerivedData、
  JNI staging 的 matrix 输出编排和 artifact manifest 仍未统一到该 key。
- 同 key 在途任务合并、取消/共享构建所有权、预热和 cache hit 仍未实现；不能把 context key
  写成构建已完成或已复用的证明。
- `build.rs`/Gradle/NDK/Xcode 隐藏输入仍受现有严格 snapshot/保守拒绝边界约束。
- 完整 steps/cleanup report、真实 viewport/DPI 和 macOS+iOS simulator+Android emulator
  连续矩阵验收仍未完成。
