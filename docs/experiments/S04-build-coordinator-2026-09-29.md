# S04：BuildKey coordinator 与在途任务合并（2026-09-29）

状态：`in_progress`。PR #177 已 squash 合并为 `5f6859d`。本切片把 #175 的 output ownership
record 推进到普通 `build`/`run` 的跨进程 BuildKey coordinator；不把它扩大为 preview/check 已完成
接入，也不宣称取消、心跳或完整状态机已经收口。

## 实现范围

- `BuildOutputLock::try_acquire` 以非阻塞方式选出同一 output root 的单一 coordinator leader；
  leader 在持有输出锁期间原子发布 `.build-coordinator.json`，记录 platform、BuildKey、attempt、
  PID、owner、时间和 `building/succeeded/failed` 状态。
- leader 只执行一次实际 build closure。成功状态只有在普通 artifact manifest 完整读取、BuildKey/
  platform 绑定和逐文件 size/hash 验证通过后才发布；follower 在看到 succeeded 后再次验证 manifest。
- follower 为当前 attempt 创建 `.build-subscribers/<token>.json`，并持有该文件的 OS 排他锁；
  这使失败结果只共享给仍在等待的调用者，也让进程崩溃留下的 subscriber 文件可被后续进程识别为
  stale 并清理。failed attempt 在没有活跃 subscriber 后允许新的调用重新选举 leader。
- leader 释放输出锁而未发布终态时，follower 取得锁后标记 abandoned 并重新选举；leader/进程退出
  后不会把半成品当成功结果。
- `gpui build`/`gpui run` 的 desktop、iOS 和 Android 可复用路径接入 coordinator；含 `build.rs`、
  release/custom/signing-sensitive 或工具链身份不可读等 cache reuse disabled 原因的路径继续使用
  原有独占锁，不共享未建模输入。
- cache clean 继续排除 owner/coordinator metadata 大小，并在发现活跃 subscriber 时跳过该 key，
  避免清理破坏等待中的共享结果。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（348 个单元测试及全部集成/协议测试）
- `cargo x check-design-docs`
- `git diff --check`

新增/覆盖的回归包括：同进程并发、跨进程同 key 只执行一次、成功 manifest 复用、失败共享、无
subscriber 失败重试、leader 消失接管、subscriber OS 锁和 cache clean 保护。PR #177 的 required
CI 最终全部通过；Android template 的一次失败是 follower cache-hit 文本契约，修复后重跑通过；
Windows live-feedback 一次既有时序波动重跑通过。没有发布或创建 tag。

## 未覆盖与下一步

当前 coordinator 尚未接入 live preview/check 的构建入口，尚无调用者取消引用计数、无“最后一个
subscriber 退出才终止”策略、heartbeat/fencing、queued/cancelled/partial 状态或有界预热。下一步
先复用该 coordinator 接入 preview/check，再补取消引用和失败/取消/partial 证据。
