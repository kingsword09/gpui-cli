# O04：observe 提交与能力降级（2026-09-24）

状态：本子 PR 新增 `gpui dev observe` 的 operation 提交、版本锁定和
coordinator 消费路径。当前 runtime 没有截图或语义 provider，因此它只会以
明确的 `unavailable` 终态结束，不会产生伪造的 observation 或 artifact。

## 已实现

- `gpui dev observe [--sync] [--window ID] [--require CAPABILITY] [--timeout DURATION]`
  提交 version-bound operation；默认等待终态，`--async` 只返回 operation snapshot；
- 提交时主动扫描项目输入，记录目标 source/asset revision、input hash 与
  `tracked_scan` 一致性；
- 多窗口不带 `--window` 返回 `ambiguous_window`；已存在窗口集合中不存在的 ID
  返回 `unknown_window`；
- `screenshot` 与 `semantics` alias 被解析为确定的 capture/semantics capability；
  非法 requirement 被拒绝；
- live coordinator 启动 operation 并检查 capability。当前 `capture.scene`、
  `capture.window`、`capture.device` 与 `semantics.read` 均声明
  `backend_unsupported`，所以请求记录 `operation.finished/failed/unavailable`；
- `--sync` 不会在 capability 已明确不可用时触发多余 build，符合 observe 算法先做
  capability 校验的步骤。

## 证据

- control 集成测试覆盖 observe 的主动扫描、operation 事件、版本/input hash 绑定与
  缺 capture provider 的明确失败；
- CLI timeout 解析测试覆盖 1ms–120s 的总 deadline 范围；
- workspace 测试、clippy、格式和设计文档检查通过。

## 边界

此功能已经提供可信失败与查询/取消入口，但尚未实现 successful observe。
后续切片必须先接入真实 scene/window/device capture provider；随后 coordinator 才能在
`--sync` 下等待 build/run/window/scene，发布不可变 observation 并关联已验证产物。
