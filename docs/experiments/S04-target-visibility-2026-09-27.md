# S04：动作目标可见性与遮挡语义（2026-09-27）

状态：已交付 devserver query/admission 的静态可见性契约；真实 macOS 窗口中的透明
overlay、系统遮挡和人工输入污染仍未运行。

## 实现范围

- action target query 请求并投影 `clip_bounds` 与可选 `obscured` 字段；diff 的语义字段
  集合也保留 `obscured` 变化。
- 当 provider 提供有效 `clip_bounds` 时，服务端计算目标 bounds 与 clip 的交集，并要求
  计划使用的中心命中点位于可见区域；无交集或中心在 clip 外返回 `element_not_visible`。
- 当 provider 明确写入 `obscured: true` 时返回 `element_obscured`，不直接调用业务 handler。
- 缺少 clip/obscured 字段或字段为 null 时保持“可见性未知”，不从截图、bounds 或字段
  缺失推断成功；真实 provider 仍必须提供可审计的遮挡来源。

## 验证

- action validator 覆盖显式 obscured、完全 clip、中心被 clip 排除和完整可见矩形。
- owner-bound VirtualList scroll/click integration 在新增字段请求下继续通过；缺少可选
  字段不会破坏已声明的正常 runtime fixture。
- workspace clippy/test 与 design-doc check 继续作为切片门槛。

## 未覆盖

尚未在真实 macOS GPUI 窗口创建透明阻挡层、测量系统窗口遮挡或注入人工输入。真实
macOS window capture 的独立证据见
[S04 macOS window evidence](S04-macos-window-evidence-2026-09-27.md)；其中语义树和动作
仍因 `a11y_inactive`/缺少 scene epoch 而明确不可用。`clip_bounds`/`obscured` 只是
provider 到 admission 的契约，不能替代原生 hit-test 或屏幕 capture 证明。
