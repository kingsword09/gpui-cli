# S04：subscriber attempt identity 与 active count（2026-09-30）

状态：PR #189 已 squash 合并为 `e6aeb62`。本切片修复 coordinator subscriber 在 Windows 等
平台被 OS 排他锁持有时不可读取内容的问题，并将 active-subscriber 判定改为锁安全的计数路径。

## 实现范围

- subscriber 文件名包含 attempt id 的安全 hex 编码前缀；active count 先按前缀隔离不同 build/preview
  attempt，再对匹配文件执行 OS lock probe，不读取仍被锁持有的 JSON 内容。
- 可重新取得锁的 stale subscriber 会被清理；无法取得锁的匹配文件计为 active 引用。不同 attempt
  的 subscriber 不会互相阻止 failed-attempt 重试。
- failed state 的共享判断使用 active subscriber count；leader/follower 生命周期回归都验证 count
  在构建中、follower 取消后和 terminal return 后的变化。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（353 个单元测试及全部集成/协议测试）
- `git diff --check`

PR #189 两组全平台 CI、desktop/android template、baseline 和文档检查全部通过；此前 Windows
locked-subscriber 时序误判已被该切片覆盖。

## 未覆盖与下一步

active count 目前用于失败共享和生命周期证据，尚未驱动“最后引用退出时自动取消 leader”、heartbeat/
fencing 或 queued/cancelled/partial 状态。下一项继续补安全的最后引用退出判定。未发布版本，未创建 tag。
