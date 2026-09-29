# S04：iOS simulator preview BuildKey coordinator（2026-09-30）

状态：PR #181 已 squash 合并为 `3cf9451`。本切片把 iOS simulator live preview 构建接入
BuildKey preview coordinator；physical device、cache-disabled 和缺少可复用 BuildKey 的路径仍走
原有 output lock/构建流程。本记录不代表 Android preview coordinator 或完整三端真实设备验收已完成。

## 实现范围

- simulator preview 对同一 iOS BuildKey 共享一个 preview attempt；leader 在同一 output lock 内执行
  Rust/Cargo、XcodeGen 和 `xcodebuild`，成功后发布绑定 iOS/BuildKey 的完整 `.app` preview manifest。
- follower 等待 attempt 终态，并重新验证 preview manifest 的逐文件 size/hash、platform/BuildKey
  绑定和当前 simulator app bundle 根；manifest 缺失、篡改或 bundle 根不匹配都不会被接受为成功。
- Cargo 构建失败共享为 `BuildFailed`，构建期间输入变化共享为 `Superseded`；基础设施/Xcode 工具
  错误继续作为真实错误返回。
- physical device 不进入 preview coordinator；cache reuse 被输入策略禁用或没有 BuildKey 时，
  继续使用原有独占 output lock 路径，不伪造跨调用者共享结果。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（350 个单元测试及全部集成/协议测试）
- `git diff --check`

新增回归验证完整 iOS app bundle manifest 的成功路径、错误 bundle 根和文件篡改均被拒绝。
PR #181 required CI 首次运行中 macOS capture helper deadline 与 Windows matrix panic 测试出现
既有时序波动；仅重跑失败 jobs 后，macOS/Windows/Linux、desktop-template、android-template 和
baseline-driver 全部通过。

## 未覆盖与下一步

Android preview 尚未接入 coordinator；调用者取消引用、heartbeat/fencing、queued/cancelled/partial
状态和隐藏构建输入发现也未在本切片实现。下一项接入 Android default-debug preview，同时保留
release/custom-signing 与 BuildKey 不可复用路径的现有策略。未发布版本，未创建 tag。
