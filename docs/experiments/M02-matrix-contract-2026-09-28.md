# M02：local matrix contract and scheduler（2026-09-28）

状态：in_progress。本切片先固定本地 matrix 的 cell lifecycle、deadline、fail-fast、
required/optional 汇总和 artifact 保留接口；当前已加入配置展开和分发前 admission，
尚未把真实 iOS/Android runner 接入 matrix CLI。

## 已交付

- `MatrixPlan` 校验非空 plan/cell、唯一 cell id、max_parallel 上限和 global/per-cell timeout。
- `MatrixScheduler` 按 plan 顺序确定性 dispatch，区分 queued、running、cancel_requested 和
  terminal 状态；active cell 的取消必须先经过 cleanup，再以 cancelled 完成。
- global/per-cell deadline 会请求取消，不会在 cleanup 尚未结束时伪造 terminal success；
  fail_fast 取消未开始 cell 并请求 active cell cleanup。
- required cell 的 failed/cancelled/inconclusive/unavailable 都不能汇总为 passed；required
  全部通过但 optional cell 缺失或失败时汇总为 partial，并保留 cell artifact ids。
- `MatrixCellRunner` execution boundary 已接入：runner error 变成 failed cell，cleanup error
  覆盖原本的 passed，cell deadline 传入 adapter；当前 harness 仍按序执行，保留后续并行
  executor 的相同 report contract。
- executor 会在 cleanup 后再次检查 deadline；即使 adapter 迟到地返回 passed，cell 仍记为
  cancelled，已有 artifact id 保留供诊断。Windows CI 曾暴露一项既有 heartbeat/semantics
  socket 测试的帧顺序假设，测试现会有界地应答 heartbeat 并继续等目标 semantics query。
- `matrix.toml` admission 会校验 frozen source、target/scenario 引用、required/timeout、
  platform-specific device/ABI 约束，并展开稳定的 target×scenario cell。
- admission 会把项目目标、runner/宿主平台、设备 ABI、场景 capture requirement 和有界
  toolchain probe 汇总为 cell 级 unavailable 原因；Windows 等不可运行目标不会被省略。
- scheduler 接受 admission 产生的 unavailable cell，不启动 runner、不执行 cleanup，但在
  report 中保留 required/optional、错误码和后续聚合语义。
- cell 可携带有界、规范化的 resource ids；matrix resource pool 在同一 supervisor 内让
  共享设备资源串行、独立目标并行，并把等待纳入 cell deadline。
- 新的 parallel executor 为每个 cell 创建独立 runner，按 max_parallel 收集结果，保留
  runner error、迟到成功、cleanup failure 和 artifact ownership 语义；旧的顺序 adapter
  继续作为测试/兼容入口。
- mobile matrix adapter 已把现有 iOS simulator/Android runner 接到统一 cell 生命周期：
  prepare 前建立可 fencing 的 run identity，随后执行 launch、设备 PNG capture 和 native
  logs；process identity/log assignment 不确定时为 inconclusive，capture artifact 仍保留。
- mobile cleanup 即使 prepare/launch/capture 中途失败也会尝试 stop_owned，随后释放本次
  host lease；它不关闭用户启动的 simulator/emulator，也不把 lease 丢失当作成功。
- gpui check --matrix <file> 已接入 admission、并行 executor 和本机 desktop scenario
  runner；它输出完整 matrix report，required 非 passed 会以失败退出。当前移动 cell 只有
  设备截图/原生日志 runner，缺少 semantics/input/reset 的 scenario driver 时明确返回
  unavailable，不把设备截图当作完整 check 通过。

## 尚未覆盖

移动端 scenario step driver、同一冻结快照构建、远程 runner 和完整 macOS+iOS simulator+
Android emulator 真实矩阵证据属于后续 M02 子 PR。
当前 resource pool 只负责单一 supervisor 的调度互斥，不替代 host-shared
DeviceLeaseSession；真实设备竞争仍必须经过 OS lease、fencing 和 runner cleanup。当前还
没有完整 macOS+iOS simulator+Android emulator 运行证据。
