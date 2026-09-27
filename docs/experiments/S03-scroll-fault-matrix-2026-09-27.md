# S03：scroll/action 故障矩阵（2026-09-27）

状态：已交付 runtime 与 devserver 的动作故障语义，以及 app-channel/window-owner 边界
测试。真实 macOS 窗口中的遮挡、人工输入污染和原生 provider 故障仍未运行。

## 实现范围

- runtime 在接收 action frame 时拒绝已过 deadline 的 click、scroll、type_text 和 key；
  pointer UI-thread 入口再次检查 deadline，避免已排队的 click/瞬时 scroll 越过最后一道
  fence。
- queued action 在投递前按 run/revision/build、window lifecycle、scene epoch、owner
  connection 和 deadline 重新 fencing。失败动作不进入 app channel，也不重放。
- 已标记为 delivered 的动作在 app-channel 断线、无结果超时、取消或目标事件未确认时
  进入 `unknown`；即使之后收到迟到结果，也不会再次执行或改写终态。
- runtime 明确报告 `dispatched=false` 时为确定性 `failed`；`dispatched=true` 但
  `target_event_received=false` 时为 `unknown`。不匹配的 connection/window/logical_id
  结果不会消费当前 delivery，正确 owner 的结果仍可完成动作。

## 验证矩阵

| 边界 | 预期 | 证据 |
| --- | --- | --- |
| queued：旧 run、旧 scene、错误 owner、关闭/失效窗口、deadline | `failed`，无 app-channel dispatch | `action_fault_tests::queued_action_is_fenced...` |
| delivered：app-channel disconnect | `unknown`，禁止 late result/replay | `delivered_action_disconnect...` 与 app-channel integration |
| delivered：deadline、cancel | `unknown`，不转成 `timed_out`/`cancelled` | `delivered_action_timeout...`、`cancelling_delivered...` |
| runtime dispatch failure | `failed` | `runtime_result_maps_dispatch_failure...` |
| target event miss | `unknown` | `runtime_result_maps...` 与 pointer integration |
| expired runtime frame | 不进入 UI action queue | `live::tests::expired_pointer_dispatches...` |

## 未覆盖

本片的真实连接测试使用 bounded app-channel 与 observation/window fixture，证明传输和
终态语义；尚未在原生 macOS 窗口中注入透明遮挡层、UI 线程阻塞、系统休眠、用户人工输入
或触控板惯性，也未证明业务滚动偏移达到目标。上述场景必须在真实窗口 provider 可用后
单独采集窗口、事件和观察证据，不能用本片的 protocol fixture 冒充通过。
