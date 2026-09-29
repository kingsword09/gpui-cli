# S04：coordinator leader subscriber 引用（2026-09-30）

状态：PR #187 已 squash 合并为 `3cacfa7`。本切片让每个 elected leader 从发布 `building` 状态前
开始持有一个 OS-locked subscriber 引用，直到 terminal state 发布并从 coordinator 函数返回。

## 实现范围

- leader 使用与 follower 相同的 `.build-subscribers/<token>.json` 生命周期；leader subscriber 的
  attempt id 与状态记录绑定，文件锁是活跃引用的权威。
- failed attempt 的共享/重试判断现在包含仍在返回的 leader；leader 尚未释放引用时新的调用会共享
  失败，leader 返回后引用释放，之后的新调用才可重新选举。
- leader 崩溃时 OS 自动释放 subscriber 锁，后续 coordinator 可清理 stale entry 并接管；成功/失败
  terminal state 写入仍在释放 output lock 前完成。
- follower 取消仍只释放自己的引用，不终止 leader；本切片不实现最后引用退出时的自动终止。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（353 个单元测试及全部集成/协议测试）
- `git diff --check`

新增回归验证 leader 构建期间 subscriber 存在、terminal state 返回后引用释放，并验证取消 follower
释放自身引用而保留 leader 引用。PR #187 两组全平台 CI、模板、baseline 和文档检查均通过。

## 未覆盖与下一步

当前 subscriber 只是生命周期证据，不是完整引用计数 API；leader 仍没有 heartbeat/fencing，最后
一个引用退出不会自动取消构建，也没有 queued/cancelled/partial 状态。下一步基于该 leader/follower
引用边界实现安全的 active-subscriber 计数与 leader 取消判定。未发布版本，未创建 tag。
