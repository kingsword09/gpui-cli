# S03：duration scroll 分段执行（2026-09-27）

状态：生成 runtime 已支持有界 duration scroll；瞬时滚动和 duration scroll 都经过正常
GPUI `ScrollWheel` 路径。通用滚动容器的注册、平台滚动惯性和真实窗口故障矩阵仍未完成。

## 实现范围

- `ScrollDispatch` 携带 `duration_ms`；服务端允许 duration 不超过动作 deadline，仍校验
  observation/run/revision/scene/owner connection fencing。
- runtime 将 duration 按目标约 16ms、最多 240 段拆分；每段通过 UI 线程的
  `Window::dispatch_event(ScrollWheel)` 投递，最后一段不晚于动作 deadline。负数和非整除
  milli-pixel delta 采用整数边界分配，所有分段之和与 admission 后的总 delta 完全相等。
- 已投递但在 deadline/窗口故障中断的动作回报 `dispatched=true` 且由服务端按未知结果处理；
  首段前即超时仍是明确失败。目标滚动 listener 收到任一段即可确认 transport hit，业务结果
  仍不冒充成功。
- `duration_ms=0` 保持原有一次性滚轮行为；旧的非零 duration admission failure 被替换为
  bounded segmented dispatch。

## 验证

- 协议 roundtrip 覆盖 duration 字段；devserver owner-bound 集成测试覆盖瞬时与 32ms
  duration dispatch、总 milli-pixel delta 和 operation completion。
- runtime 纯函数测试覆盖正负 delta、非整除分段、零时长和最大段数上限。
- 生成 macOS 模板 debug `gpui-dev` check 通过；workspace fmt/check/clippy/test 和设计文档
  检查继续通过。

## 未覆盖

尚未提供任意用户组件的通用 scroll target registry、真实平台惯性/触控板 provider、取消中途
动作的显式 API，或真实窗口断线/崩溃故障证据。回退：超出动作 deadline 或 runtime 不支持
scroll capability 时在 admission/执行边界返回明确错误，不重放部分滚动。
