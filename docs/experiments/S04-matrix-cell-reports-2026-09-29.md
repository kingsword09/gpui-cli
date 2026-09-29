# S04：matrix cell 完整 CheckReport（2026-09-29）

状态：`in_progress`。PR #167 已 squash 合并为 `5e5031a`。本切片关闭 matrix report 的摘要
投影缺口：control scenario cell 不再只保留 status、primary error、capture artifact IDs，
而是保留完整 scenario `CheckReport`；不声称真实三端连续验收已完成。

## 实现范围

- `MatrixCellExecution` 可携带完整 `CheckReport`，`MatrixCellResult.check_report` 将其写入
  `MatrixReport` JSON，同时保留原有稳定摘要字段、artifact IDs 和 `CheckContext`。
- sequential 和 parallel matrix executor 都在 cell 的 terminal summary 提交后附加完整报告，
  因此 steps、action/assertion/capture evidence、primary error、cleanup 和 runtime context
  不会在汇总时丢失。
- `CheckReport` 及其嵌套 evidence DTO 增加反序列化能力，报告可以从 MatrixReport JSON
  round-trip；admission-unavailable cell 和没有 scenario executor 的 capture-only mobile
  lifecycle cell 保持 `check_report: None`，不创建空报告冒充执行证据。

## 语义边界

`MatrixCellResult.status` 仍是 matrix scheduler 的最终 cell 状态；若外层 deadline、资源等待
或 runner cleanup 在 scenario report 生成后改变 cell 状态，摘要状态和 `check_report` 需要结合
阅读，不能把底层 scenario report 单独当成 matrix 最终结论。

本切片仍不提供：

- 跨独立命令的构建所有权、cache hit 或在途任务 coalescing；
- capture-only mobile lifecycle 的完整 scenario steps/语义输入报告；
- 真实 macOS 窗口、iOS simulator/真机、Android 设备连续矩阵验收；
- 环境、viewport/DPI、设备重连和完整 L2/L3 CI 证据。

## 验证

本地通过：

- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（333 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增回归测试验证完整 steps/error/cleanup 在 `MatrixReport` 中保留，并验证 JSON
serialize/deserialize round-trip。PR #167 的 required CI 全部通过；未发布版本，未创建 tag。

## 下一步

把同一完整证据边界接到真实移动 scenario/lifecycle 路径，并继续实现独立命令间的 build
coordinator/ownership；完成后再基于 verified manifest 定义 cache hit/coalescing 语义。
