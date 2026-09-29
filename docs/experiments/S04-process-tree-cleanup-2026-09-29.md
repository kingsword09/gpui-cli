# S04：desktop check process-tree cleanup（2026-09-29）

状态：`limited_adopt`。本记录只覆盖隔离 desktop preview 的进程归属和终止传播，
不代表 S04 的 GUI、fixture、环境或连续场景验收已完成。

## 问题

旧的 desktop check runner 直接对 preview supervisor 调用 `kill`/`wait`。这只能回收
leader，不能证明独立 preview 应用或其后代退出；历史探针曾观察到报告 cleanup 成功时测试
应用仍存活。

## 实现

- PR #151（合并提交 `5b63656`）复用 `OwnedChild`，使 check preview 在 Unix 使用独立
  process group，在 Windows 使用 Job Object。
- `OwnedChild::spawn_with_stdio` 保留 check 所需的 null stdin、null stdout 和 inherited
  stderr，不把 check 的生命周期修复建立在未消费的输出管道上。
- `DesktopCheckRunner` 的 launch failure、显式 cleanup 和 Drop 都终止同一个 owned tree。

## 验证

- 新增 `custom_owned_child_cleanup_closes_descendant_pipes`：parent leader 启动 helper，
  helper 持有 stdout/stderr 管道；终止 owned child 后两条管道在 5 秒内关闭，并保留 parent
  输出证据。
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`：360 passed，0 failed
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`
- PR #151 required CI 全部通过；一次既有 Windows asset-reconciliation 并发测试波动重跑后通过。

## 边界

该测试证明 owned process group/job 的终止传播和管道关闭，不替代一次真实 GPUI check
GUI 会话的 cleanup/进程身份探针。移动 runner、用户从 preview 自行脱离的外部进程、fixture
hash、环境确定性、冻结构建和完整 S04 连续执行仍需单独验收。
