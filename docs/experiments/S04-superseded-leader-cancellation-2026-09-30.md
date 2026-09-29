# S04：superseded BuildKey coordinator leader 取消（2026-09-30）

状态：PR #191 已 squash 合并为 `16cf04b`。本切片让 preview coordinator leader 在自身
Build revision 失效时终止正在运行的 owned process tree，并让等待旧 attempt 的 follower
释放旧引用、重新选举；它不把 follower 的单独取消传播给 leader。

## 实现范围

- `Build::is_current_revision` 读取 session 最近观察到的 desired revision，供进程监视循环低成本轮询；
  发现 revision 失效时，`output::run` 通过 `OwnedChild` 终止整个子进程树并返回 superseded 结果。
- coordinator 在 leader 工作结束后再次检查取消谓词。superseded leader 发布明确的 retryable
  terminal error；等待者将其视为 abandoned attempt，释放旧 subscription 并从最新 coordinator state
  重新加入/竞争，而不验证或返回 superseded attempt 的产物。
- follower cancellation 仍只影响该 follower。leader 自己的 subscriber 保持到 terminal state 发布并
  返回；若 leader 的 Build revision 仍 current，即使没有 follower，它也可完成自己需要的构建。
- `.build-coordinator.lock` 现在串行化 subscriber 注册、active count、coordinator record 发布和
  cache-clean 的活跃订阅检查/内容清理，避免探测器把刚创建但尚未持有 subscriber lock 的文件清理掉。
- superseded 仍通过 `failed` state 加专用错误标记表达，不新增 `cancelled` state；active count 继续按
  attempt 隔离，并用于判断普通失败是否仍应共享。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（356 个单元测试及全部集成/协议测试）
- `cargo x check-design-docs`
- `git diff --check`

新增回归验证 superseded 后子进程树被终止、leader 发布 retryable 状态、follower 重新接管，以及
跨进程 leader/follower 和 locked subscriber 的确定性握手。PR 与 push 两套全平台检查、desktop/android
template、baseline 和设计文档门槛均通过。

## 未覆盖与下一步

本切片不是完整的引用归零取消状态机：coordinator 没有独立 `cancelled`/`partial` terminal state，
不会因某个 follower 退出而取消仍 current 的 leader，也没有 heartbeat/fencing。当前 active count
负责 attempt-bound 活跃引用证据、普通失败共享和 cache-clean 保护；leader 的失效依据仍是自己的
Build revision，旧 attempt 的 followers 在 superseded 后重新竞争。下一步补明确的 terminal-state 与
last-reference policy，再继续 heartbeat/fencing、隐藏构建输入和剩余 preview/check orchestration。
未发布版本，未创建 tag。
