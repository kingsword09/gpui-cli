# S04：fixture identity after preview reset（2026-09-29）

状态：`limited_adopt`。本记录覆盖 reset 后 fixture identity 的 runtime/report 传播，
不代表 S04 的真实 GUI、矩阵冻结或连续场景验收已完成。

## 问题

Preview reset 会重读 fixture JSON 并递增 generation，但旧 runtime 保留启动时的
`fixture_hash`。因此 fixture 从初值 0 改为 42 后，`scenario_ready` 和单场景 check report
仍可能引用旧 hash，无法把观察/基线绑定到实际 reset 内容。

## 实现

- PR #153（合并提交 `3e8b874`）在生成模板 reset 路径按当前 fixture bytes 重新计算 SHA-256。
- 新的 `scenario_ready` 事件携带刷新后的 hash。
- `ScenarioRunner` 暴露 runtime fixture identity；desktop executor 在报告收尾和早期失败路径
  用 runtime hash 覆盖启动校验 hash，无法观察 runtime identity 的 runner 保留 launch hash。

## 验证

- `execute_reports_runtime_fixture_hash_over_the_launch_hash` 回归测试通过。
- 模板 SHA-256 标准向量测试通过（debug preview runtime）。
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`：361 passed，0 failed
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`
- PR #153 required CI 全部通过。

## 边界

本切片没有重跑完整真实 GUI reset probe，也没有将 matrix cell report 扩展为保留完整
fixture/context identity；check/matrix 同一冻结快照、环境/viewport/DPI 控制和三夹具连续
验收仍属于后续切片。
