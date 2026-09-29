# S04：coordinator 最后引用取消边界（2026-09-30）

状态：PR #193 已 squash 合并为 `4b1a309`。本切片在 coordinator 层实现独立的
caller-cancel 语义：leader 释放自己的 subscriber 后，在同一 coordinator state lock 内统计剩余
active references；有 follower 时保留 terminal result 供其消费，无 follower 时发布 retryable
cancellation marker 并允许下一调用重新竞争。

## 实现范围

- 新增 `CallerCancelled` cancellation reason 和
  `coordinate_preview_build_with_verifier_and_caller_cancel` API。现有
  `Build::is_current()` preview 路径继续使用 `Superseded` reason，不改变旧 attempt 的重建语义。
- leader caller 取消时，先取得 `.build-coordinator.lock`，释放自己的 OS-locked subscriber，按 attempt
  统计剩余引用，再发布终态；follower 无法在“零引用判断”和终态发布之间插入新的可消费状态。
- 没有剩余 follower 时写入 `failed` state 与专用
  `coordinated BuildKey leader cancelled without subscribers` marker；该 marker 不向后续调用者
  共享失败，而是让它重新选举 leader。失败/取消产物不因此成为 cache hit。
- 仍有 follower 时，leader caller 只收到 `coordinated BuildKey subscriber cancelled`；成功或失败的
  terminal result 原样发布，follower 仍可作为 `Follower` 完成。leader 的 subscriber 已释放，不会阻止
  follower 完成或让 cache-clean 错误保留引用。
- superseded cancellation 仍强制旧 attempt 进入 retryable superseded marker，followers 重新竞争；
  这与 caller-cancel 的“共享 terminal result”策略明确分离。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（358 个单元测试及全部集成/协议测试）
- `cargo x check-design-docs`
- `git diff --check`

新增回归覆盖：无 follower 的 retryable marker 与下一次 leader 接管；有 follower 时取消 leader
仍保留共享成功结果；原有 superseded、跨进程、Windows-safe subscriber 和 cache-clean 测试继续通过。
PR 与 push 两套全平台检查、desktop/android template、baseline 和设计文档门槛均通过。

## 未覆盖与下一步

该 API 是 coordinator 层的引用语义，不等于完整 live preview 取消接线：当前 live `Build::is_current()`
仍按 superseded 路径处理 revision 变化；caller-cancel API 尚未接入每个 preview/check orchestration
入口。构建闭包若没有自己的 cooperative polling，也不会因最后引用归零自动杀进程；本切片没有新增
独立 `cancelled`/`partial` state，仍使用 retryable marker。下一步接入真实调用者取消与 owned process
终止，再补独立 terminal-state、heartbeat/fencing 和隐藏构建输入。未发布版本，未创建 tag。
