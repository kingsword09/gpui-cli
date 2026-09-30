# S04：BuildKey coordinator 显式 Partial 终态（2026-09-30）

状态：PR #199 已 squash 合并为 `0e05087`。本切片增加显式 `partial` terminal state，用于说明
attempt 留下了不完整或不安全的输出；它是诊断/重试状态，不授予任何消费者使用部分产物的权限。

## 实现范围

- `BuildCoordinatorState` 现包含 `Building`、`Succeeded`、`Failed`、`Partial`、`Cancelled`。
- build closure 的 error chain 含精确的 `BUILD_COORDINATOR_PARTIAL_ERROR` marker 时，terminal record
  写入 `Partial`；普通编译错误仍为 `Failed`，显式无引用 caller-cancel 仍为 `Cancelled`。
- leader 将原始 partial error 返回 caller，record 保留诊断信息。partial attempt 不执行成功结果路径；
  后续调用忽略该终态并创建新 attempt。
- 已等待中的 follower 观察到 `Partial` 后释放旧 subscription、放弃旧 attempt 并重新选举；即使旧
  attempt 留下可通过完整 manifest verifier 的文件，follower 也不会验证/消费它。
- 成功 manifest、普通失败共享、取消重试和 superseded 语义不变。

## 验证

- `cargo test --workspace --locked`：362 个单元测试及全部集成/协议测试通过。
- `cargo fmt --all -- --check`、workspace clippy、`cargo x check-design-docs`、`git diff --check` 通过。
- PR 与 push 两套 CI 最终全部通过；PR 首轮 Windows 的既有
  `same_key_builders_share_one_in_flight_attempt` 时序测试失败，失败 job 重跑后 Windows/macOS/Linux、
  desktop-template、android-template 和 baseline-driver 均通过。
- PR #199 以 squash 合并；未发布版本，未创建 tag。

## 未覆盖与下一步

当前 marker 由调用方显式返回；coordinator 不会扫描任意输出目录推断“部分产物”，也没有定义可消费的
partial artifact manifest。partial 输出只会被下一次 leader 重建/覆盖，不提供恢复或渐进复用。heartbeat/
fencing、隐藏构建输入、物理 iOS 和 Android 自定义/发布签名路径仍未完成。下一切片推进 coordinator
owner heartbeat/fencing，并明确它与 output OS lock/subscriber 活性判断的关系。未发布版本，未创建 tag。
