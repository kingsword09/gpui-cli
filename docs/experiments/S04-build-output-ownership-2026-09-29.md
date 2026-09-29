# S04：BuildKey 输出 ownership record（2026-09-29）

状态：`in_progress`。PR #175 已 squash 合并为 `14339b5`。本切片把已有的
`BuildOutputLock` 从“只有 OS 锁”推进到“锁内有可审计的当前 owner 记录”，但不把记录扩大解释为
跨命令 coordinator 或同 key 在途任务合并。

## 实现

- `BuildOutputLock::acquire` 和 preview 使用的 `acquire_at_root` 在成功取得 OS 文件锁后，原子写入
  output root 下的 `.build-owner.json`；记录包含 `schema_version`、唯一 `owner_id`、`pid`、
  `started_at_ms`、`state = "building"`，以及可用时的 `key_hash`。
- owner record 通过同目录临时文件写入、`sync_all` 后原子发布；取得 OS 锁是写入前提，因此不会让
  未持锁进程宣称自己拥有输出根。
- guard drop 只在读取到的 `owner_id` 仍与自身匹配时删除 record。进程崩溃留下的 stale record
  不被单独解释为活跃；下一个进程成功取得 OS 锁后会原子覆盖它。
- cache clean 获取同一输出根的锁并发布 owner record；`.build-owner.json` 被排除在 cache size
  统计之外，清理保留 lock root 以便后续 waiters 继续使用。

## 保证与边界

这份 record 是跨进程 ownership 证据，方便诊断“当前/上一次谁在处理该 BuildKey 输出根”。
OS 文件锁仍是活跃性的唯一权威。当前不提供：

- coordinator、同 BuildKey 在途任务共享或 subscriber/ref-count；
- 调用者取消时只在无订阅者后终止构建；
- heartbeat、lease expiration、fencing token 或失败/取消/partial 状态机；
- `build.rs`、Gradle、NDK、Xcode 隐藏输入的完整建模；
- 真实桌面/iOS/Android 连续矩阵验收。

## 验证

PR #175 本地和 required CI 均通过：

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（340 个单元测试及全部集成/协议测试）
- `cargo x check-design-docs`
- `git diff --check`

新增回归覆盖：持锁时 record 字段、guard drop 的匹配删除、stale record 覆盖、preview root lock
路径和 cache clean 对 owner metadata 的大小排除。该验证证明记录的发布/清理语义，不替代崩溃恢复
竞争、coordinator 共享结果或真实三端构建验收。

## 下一步

在这个 output ownership 边界上实现真正的 BuildKey coordinator：独立 `build`/`run`/`check`/preview
请求对同一 key 共享一个在途任务和已验证结果；随后再加入调用者订阅/取消引用以及失败、取消、
partial artifact 的状态和证据。
