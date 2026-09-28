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

## 尚未覆盖

真实 platform runner dispatch、同一冻结快照构建、host/ABI/toolchain admission、设备资源锁、
跨目标并行和 Windows unavailable cell 属于后续 M02 子 PR。
