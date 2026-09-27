# S03：通用滚动容器显式声明契约（2026-09-27）

状态：已交付 runtime 与 devserver 的通用 admission 契约；真实窗口断线/崩溃故障矩阵、
平台滚动惯性和触控板 provider 仍待后续切片验证。

## 实现范围

- 生成 runtime 提供 `declare_scroll_target(logical_id)`。声明按渲染帧保存，帧开始时清理，
  避免已卸载或已替换的容器继续出现在观察结果中。
- `record_scroll_target_bounds` 记录当前 prepaint bounds；`confirm_scroll_target_hit`
  只在 GPUI 的正常 `on_scroll_wheel` 路径收到事件后确认 transport hit，不把业务滚动
  偏移误报为动作成功。
- semantics enrichment 将显式声明的节点导出为 `scrollable: true`。devserver 的 Scroll
  query 同时请求该字段，并在投递前拒绝缺少声明的唯一节点，返回
  `element_not_scrollable`。
- VirtualList viewport 使用该通用接口；fake semantics integration fixture 也明确包含
  `scrollable: true`，防止测试绕过 admission 契约。

## 验证

- action validator 单元测试覆盖未声明节点拒绝和显式声明通过。
- runtime enrichment 测试覆盖声明导出 `scrollable: true`，以及下一帧清理后不再导出。
- devserver owner-bound scroll integration test 继续覆盖瞬时与 duration scroll dispatch，
  并使用显式 `scrollable` fixture 字段。

## 未覆盖

真实 GPUI/macOS 窗口中的断线、超时、崩溃、覆盖层和触控板/惯性故障证据仍未完成；
本契约只证明“目标明确声明且 wheel 事件进入该容器”，不证明业务滚动偏移已达到预期。
