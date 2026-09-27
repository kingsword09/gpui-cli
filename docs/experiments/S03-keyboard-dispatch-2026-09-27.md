# S03：keyboard 输入路由（2026-09-27）

状态：`type_text` 和 `key` 已接入生成 LoginForm 的正常 GPUI focus/key event 路径；
通用组件键盘适配、真实窗口故障矩阵和移动端 IME 仍未完成。本切片不直接调用业务
handler，也不把服务端提交成功当成业务结果。

## 实现范围

- app-channel 新增 `text_dispatch` 和 `key_dispatch`，复用现有 observation、window、
  scene epoch、run/revision、owner connection 和 operation 幂等 fencing。
- runtime 在 UI 线程将动作目标 focus 到显式注册的 `FocusHandle`，再通过
  `Window::dispatch_keystroke` 发送普通 GPUI key-down/模拟 IME 输入；`replace` 先发送
  platform-secondary select-all，`append` 保留当前文本。
- 生成 LoginForm 的 username/password 节点导出实际 prepaint bounds/enabled，注册稳定
  keyboard target；文本、Backspace、Enter 由节点的 `on_key_down` 处理，Enter 使用 fixture
  错误响应。动作结果只有在目标 key event observer 收到事件后才确认，业务状态仍可标为
  `unverified`。
- hello/state 能力拆为 `input.keyboard.type_text`、`input.keyboard.key` 和聚合的
  `input.keyboard`；不支持的旧 runtime 在 admission 阶段返回 unavailable。
- 空 append 文本、NUL、超长文本、非法 mode/key 和已过 deadline 的 runtime 队列请求均
  在执行前拒绝；已有 pointer/scroll 结果与 failure/unknown 语义保持不变。

## 验证

- protocol roundtrip 覆盖 `TextDispatch`/`KeyDispatch` wire shape。
- devserver owner-bound integration test 覆盖 text/key dispatch、ActionResult 完成和
  keyboard capabilities；既有 pointer click、scroll、run/scene fencing 测试继续通过。
- `cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo test --workspace --locked` 通过；一次并发模板构建期间的独立 toolchain probe
  测试瞬态失败，单独重跑通过。
- 生成 macOS 模板 `gpui-dev` debug/locked check 和 release check 通过；release +
  `gpui-dev`、`gpui-dev + gpui-profile` 按设计拒绝编译。

## 未覆盖

没有实现通用文本编辑器/IME composition、selection/caret 语义、任意组件 focus 注册、
分段滚动 duration、移动端软键盘或真实窗口断线/崩溃故障证据。回退：runtime 不声明
keyboard capability 时 CLI 返回 unavailable，不重放也不直接修改业务对象。
