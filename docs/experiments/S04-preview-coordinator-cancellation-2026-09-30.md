# S04：preview coordinator follower 取消与 subscriber 释放（2026-09-30）

状态：PR #185 已 squash 合并为 `dada424`。本切片为 desktop、iOS simulator 和 Android
default-debug preview 的 coordinator wait 增加调用者取消检查：已 superseded 的 follower 可以
停止等待并释放 subscriber；leader attempt 继续运行并可向其他仍在等待的 follower 发布结果。

## 实现范围

- 新增 preview coordinator 的 cancellation predicate；等待 building attempt 的 follower 每轮轮询
  检查该 predicate，取消后删除 `.build-subscribers/<token>.json` 并返回明确的 coordinator-cancelled
  错误。
- 三个 live preview 构建入口都将 `!Build::is_current()` 作为取消条件，并把该错误映射为现有
  `Iteration::Superseded`；普通 build/run coordinator API 保持原有行为。
- 取消 follower 不修改 coordinator state、不抢占 output lock、不终止 leader；这为后续“最后一个
  subscriber 退出才终止构建”语义保留了安全边界，但本切片尚未实现 leader 引用计数或终止传播。
- Windows 回归测试不读取仍被 OS 排他锁持有的 subscriber 文件内容，改为检查 locked directory
  entry 存在，避免把合法订阅误判为缺失。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（352 个单元测试及全部集成/协议测试）
- `git diff --check`

新增回归覆盖取消 follower 释放 subscriber、leader 继续完成，以及跨平台 locked subscriber 测试
观察方式。PR #185 初始 Windows 时序失败在测试修复后重跑通过；最终两组 CI 的 Linux/macOS/Windows、
desktop-template、android-template、baseline-driver 和文档检查全部通过。

## 未覆盖与下一步

当前只取消等待中的 follower；leader 无 heartbeat/fencing，最后一个 subscriber 退出时不会自动终止
构建，也没有 queued/cancelled/partial 终态或跨命令取消引用证据。下一步补 subscriber 引用计数和
leader 端安全取消/失败状态证据，再处理 heartbeat/fencing。未发布版本，未创建 tag。
