# S02：registry manifest 契约（2026-09-25）

状态：registry 静态读取、desktop 多组件 preview runtime 的基础渲染与 reset 已实现；
输入路由、真正的滚动执行和 check executor 仍在进行中。

## 实现范围

- `.gpui/registry-manifest.json` 使用 schema-v1：组件 name/version/fixture_schema、
  supports_reset、ready_ids、logical_ids 和 environments。
- `gpui scenario validate` 严格解析 manifest，拒绝未知字段、错误 schema_version、空
  必需字段和重复组件名。
- 场景需要 `scenario.reset` 时，registry 存在但组件未声明 supports_reset 会失败；ready_id
  不在组件声明中也会失败。
- 示例 manifest 与生成模板 runtime 均覆盖 Counter、LoginForm、VirtualList；custom fixture
  schema 只在 registry 缺少具体 schema 时报告 fixture_schema_unavailable。
- 生成模板增加显式 `PreviewRegistry`，应用启动时生成 `.gpui/registry-manifest.json`；默认只注册
  真实生成的三个 preview surface。LoginForm 的用户名/密码/提交/错误节点和 VirtualList 的
  viewport/稳定 key 行由 fixture 驱动构造。
- `gpui preview Counter --scenario counter-basic --target desktop` 在构建后创建
  `.gpui/previews/<run>/data`，不读取 `.gpui/sessions/`，runtime 校验 fixture 并发送
  `scenario_ready`，事件带 fixture hash、实际环境、data dir 和 `reset_generation=1`。
- `gpui dev reset --scenario counter-basic` 经 control server 和有界 app-channel 队列送入 UI
  线程；runtime 重新读取 fixture、递增 generation，发送 `scenario_reset_result` 和新的
  `scenario_ready`。旧请求不能跨 run 投递。
- `scenario.reset` 只有 preview runtime 在 hello 中声明时才进入可用 capability；普通 Live
  runtime 的 reset 请求返回 `unavailable`，不会把普通交互状态误当成场景状态。
- runtime 的 `reset_generation()`/`reset_generation_for()` 会在同一进程内递增 generation 并重新
  写入 runtime 状态；应用负责在调用该 hook 后重新创建自己的组件和取消旧异步任务。
- LoginForm 提交按钮会使用 fixture response 显示确定性的错误节点；当前没有真实
  `type_text`/键盘输入状态，VirtualList 只渲染 GPUI `uniform_list` 并报告 fixture 的初始偏移，
  真实输入和滚动动作属于 S03。

## 验证

- 场景单元测试验证合法 manifest 的 reset/ready 匹配。
- 缺少 manifest 仍只报告 registry_unavailable，不把未知组件当作已检查。
- app-channel reset 请求、run fencing、generation result 和 runtime queue wire shape 有 bounded 测试。
- workspace tests、clippy、fmt、生成 desktop 模板 debug/`gpui-dev`/locked 检查和设计文档检查通过。

## 未覆盖

LoginForm 的真实文本输入、VirtualList 的 agent 滚动动作、iOS/Android preview、旧异步任务的
真实隔离证据、check executor 和移动端环境 adapter 尚未覆盖。
