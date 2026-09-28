# M02：local matrix contract and scheduler（2026-09-28）

状态：in_progress。本切片先固定本地 matrix 的 cell lifecycle、deadline、fail-fast、
required/optional 汇总和 artifact 保留接口；尚未把真实 iOS/Android runner 接入 matrix CLI。

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

## 尚未覆盖

真实 platform runner dispatch、同一冻结快照构建、host/ABI/toolchain admission、设备资源锁、
跨目标并行和 Windows unavailable cell 属于后续 M02 子 PR。
