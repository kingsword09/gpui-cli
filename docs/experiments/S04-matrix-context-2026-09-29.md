# S04：matrix per-cell context 传播（2026-09-29）

状态：`in_progress`。PR #159 已 squash 合并为 `85e9c0d`。本切片把 scenario executor 已有的
`CheckReport.context` 接入 matrix cell/report；它不改变执行状态判定，也不把缺失的 runtime
证据伪造成可用。

## 实现范围

- `MatrixCellExecution` 携带可选 `CheckContext`，顺序和并行 executor 都在 cleanup/late-result
  处理后把它传给 scheduler。
- `MatrixCellResult.context` 序列化 reset generation、snapshot hash、BuildKey（若有）、runtime
  environment 和 uncontrolled inputs；admission-unavailable、runner error、mobile-only
  lifecycle 没有 context 时保持 `null`/省略。
- desktop/mobile control scenario runner 只转发真实 `CheckReport.context`；没有从 heartbeat、
  toolchain admission 或 native capture 推断 reset/environment/semantic 成功。
- 原有 MatrixReport status、primary error 和 capture artifact IDs 语义保持不变；本切片不加入
  完整 steps、cleanup report 或 target-specific BuildKey。

## 验证

PR #159 合并前已通过：

- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（331 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增测试验证 `snapshot_hash` 从 cell execution 传播到 MatrixReport JSON。PR #159 的 required
CI 全部通过；macOS window-capture helper 首次环境超时后，失败 job 重跑通过。该 PR 未发布
版本或 tag。

## 未覆盖与边界

- MatrixReport 仍没有每一步的 before/after observation、action/assert evidence、日志序号或
  cleanup detail；这些需要独立的有界 report schema/产物策略。
- matrix preview 已从共享 frozen input root 启动，但 target-specific BuildKey、Cargo/Gradle/
  Xcode 输出布局、共享构建和隐藏输入建模仍未完成。
- context 中的 environment 是 runtime 报告值，不等于 viewport/DPI、字体/backend、时钟、
  网络或设备状态已经被外部完全控制。
- 无 GUI/设备运行证据不会被本切片升级为真实 macOS+iOS simulator+Android emulator 矩阵验收。
