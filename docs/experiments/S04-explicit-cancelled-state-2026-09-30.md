# S04：BuildKey coordinator 显式 Cancelled 终态（2026-09-30）

状态：PR #197 已 squash 合并为 `f8a95ce`。本切片为 coordinator 持久记录增加显式
`cancelled` 状态，区分调用者取消与普通构建失败；没有改变 superseded attempt 的重试语义。

## 实现范围

- `BuildCoordinatorState` 现在包含 `Building`、`Succeeded`、`Failed`、`Cancelled`。
- terminal record 根据错误结果映射状态：成功为 `Succeeded`，无剩余 subscriber 的 caller-cancel
  marker 为 `Cancelled`，其他错误仍为 `Failed`。取消原因字符串保留在 `error` 字段，便于诊断。
- follower 观察到 `Cancelled` 后释放旧 subscription 并重新竞争；不会把取消当成可共享的编译失败，
  也不会将取消产物当作成功 cache hit。
- 普通编译错误仍发布 `Failed` 并按既有 attempt/reference 策略共享；superseded 仍发布 retryable
  `Failed` marker，旧 attempt 的 follower 重新竞争。
- 若 leader caller 取消时仍有 follower，leader 自身返回 cancellation，但 attempt 可继续并将最终
  `Succeeded`/`Failed` 结果提供给 follower；`Cancelled` 只表示没有 follower 可消费该 attempt 的
  取消终态。

## 验证

- 本地通过 359 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs 和 diff check。
- PR 与 push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver
  和文档门槛均通过。push workflow 首次 Windows 时序失败在仅重跑失败 job 后通过。
- PR #197 以 squash 合并；未发布版本，未创建 tag。

## 未覆盖与下一步

目前没有 `Partial` 终态；尚未定义或持久化“部分产物可用但 attempt 未完整成功”的可消费契约。记录仍无
heartbeat/fencing，caller-cancel 状态也不能替代进程存活证明。隐藏的 build.rs/Gradle/NDK/Xcode 输入、
iOS physical signing、Android custom/release signing-sensitive 路径仍不进入共享 coordinator。
下一步为 `Partial` 状态先明确产物与 consumer 契约并补失败/恢复测试，再推进 heartbeat/fencing 和剩余
真实平台验收。未发布版本，未创建 tag。
