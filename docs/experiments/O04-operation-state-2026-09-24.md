# O04：有界 operation 状态机（2026-09-24）

状态：本子 PR 建立 observe/检查等异步工作的共享状态边界，并接入 control
端点的查询与取消。真实 operation 提交和 capture runner 尚未接入。

## 已实现

- operation 状态为 `queued → running → succeeded/failed/cancelled/timed_out/
  superseded/unknown`，终态不可被迟到 completion 改写；
- 每个 session 最多 4 个活动 operation，查询历史最多 2048 条或 8 MiB；
- operation 有总 deadline 上限，过期工作进入 `timed_out`；
- request_id 按 kind、scope 和规范化 target 做幂等去重，参数变化返回
  `idempotency_conflict`；查询记录淘汰后仍保留有界 request tombstone，不能静默重放；
- `gpui dev operation get/cancel` 使用已有 control 身份和 session 绑定；`operation get`
  支持有界 `--wait`，服务端通过 Condvar 等待终态或 operation deadline，不让 CLI
  以固定短间隔忙轮询；
- queued、started、finished 变更写入有序事件日志，取消和迟到完成均有测试证据。

## 证据

- 纯状态机测试覆盖生命周期、deadline、幂等冲突、取消竞争、历史淘汰和活动工作保留；
- 长轮询测试覆盖运行中 operation 在终态转换后唤醒查询；
- control 集成测试覆盖查询、取消、终态稳定性及 operation 事件；
- workspace 测试、clippy、格式和设计文档检查通过。

## 边界

本切片不创建假的 observe 成功结果，也不在 control worker 中等待编译或读回。
下一步把 `observe --sync` 请求登记为 operation，并让 live coordinator 按目标
revision 驱动 build/run/资源/scene 等待；capture provider 缺失时必须返回明确的
`unavailable`，不能由本状态机填充产物。
