# S02：registry manifest 契约（2026-09-25）

状态：registry 静态读取与 desktop Counter preview runtime 首个子切片已实现；完整多组件
reset/check 仍在进行中。

## 实现范围

- `.gpui/registry-manifest.json` 使用 schema-v1：组件 name/version/fixture_schema、
  supports_reset、ready_ids、logical_ids 和 environments。
- `gpui scenario validate` 严格解析 manifest，拒绝未知字段、错误 schema_version、空
  必需字段和重复组件名。
- 场景需要 `scenario.reset` 时，registry 存在但组件未声明 supports_reset 会失败；ready_id
  不在组件声明中也会失败。
- 示例 manifest 覆盖 Counter、LoginForm、VirtualList；生成模板 runtime 只注册真实存在的
  Counter surface，custom fixture schema 只在 registry 缺少具体 schema 时报告
  fixture_schema_unavailable。
- 生成模板增加显式 `PreviewRegistry`，应用启动时生成 `.gpui/registry-manifest.json`；默认只注册
  真实生成的 Counter surface，避免把未实现的 Form/List 伪装成可运行组件。
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

## 验证

- 场景单元测试验证合法 manifest 的 reset/ready 匹配。
- 缺少 manifest 仍只报告 registry_unavailable，不把未知组件当作已检查。
- app-channel reset 请求、run fencing、generation result 和 runtime queue wire shape 有 bounded 测试。
- workspace tests、clippy、fmt、生成 desktop 模板 debug/`gpui-dev`/locked 检查和设计文档检查通过。

## 未覆盖

LoginForm/VirtualList 的真实注册与渲染、iOS/Android preview、旧异步任务的真实隔离证据、
check executor 和移动端环境 adapter 尚未覆盖。
