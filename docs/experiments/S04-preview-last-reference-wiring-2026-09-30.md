# S04：preview 最后引用取消真实接线（2026-09-30）

状态：PR #195 已 squash 合并为 `3364c58`。本切片把 #193 的 coordinator
caller-cancel/ref-count 语义接到 desktop、iOS simulator 和 Android default-debug preview 的
owned process loop。

## 实现范围

- `Build` 在 coordinated preview build 期间安装 leader control；Cargo、rustup、xcodebuild、cargo-ndk
  和 Gradle 等通过 `output::run` 轮询同一 control，而不再只读取 caller 自身的 session stopping/revision。
- caller cancellation 先在 coordinator state lock 下释放 leader subscriber 并检查剩余引用：没有
  follower 时终止整个 owned process tree、写入 retryable cancellation marker；有 follower 时不杀共享
  process，leader caller 脱离，剩余 follower 继续等待并复核 terminal manifest。
- superseded revision 仍无条件终止旧 attempt 的 owned process tree 并发布 superseded marker，followers
  重新竞争；这与 caller-cancel 的共享结果策略保持分离。
- process probe 的终止与 state record 发布共用 coordinator lock，避免“最后引用判断”与新 follower
  注册之间出现错误终态；`Build::is_current_for_coordinated_work` 在仍有 follower 时允许共享构建继续，
  在无 follower 或 superseded 时停止后续发布。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（359 个单元测试及全部集成/协议测试）
- `cargo x check-design-docs`
- `git diff --check`

新增 controlled leader cancellation 回归，验证最后引用会调用 owned process termination；既有
跨线程/跨进程、superseded、follower cancellation、Windows-safe subscriber、cache-clean 和 process-tree
测试继续通过。PR 与 push 两套全平台检查、desktop/android template、baseline 和设计文档门槛均通过。

## 未覆盖与下一步

当前仍使用 `failed` state 加 retryable marker 表达 cancellation，没有独立 `cancelled`/`partial`
terminal schema；没有 heartbeat/fencing，也没有对未建模 build.rs/Gradle/NDK/Xcode 读集的完整约束。
physical iOS、Android release/custom-signing/cache-disabled 路径不进入该共享 preview coordinator。
下一步补独立终态/heartbeat/fencing、完整移动设备 scenario 证据和真实连续验收；未发布版本，未创建 tag。
