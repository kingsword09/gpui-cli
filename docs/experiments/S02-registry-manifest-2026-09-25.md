# S02：registry manifest 契约（2026-09-25）

状态：静态读取子切片已实现；没有生成 registry、启动 preview 或执行 reset。

## 实现范围

- `.gpui/registry-manifest.json` 使用 schema-v1：组件 name/version/fixture_schema、
  supports_reset、ready_ids、logical_ids 和 environments。
- `gpui scenario validate` 严格解析 manifest，拒绝未知字段、错误 schema_version、空
  必需字段和重复组件名。
- 场景需要 `scenario.reset` 时，registry 存在但组件未声明 supports_reset 会失败；ready_id
  不在组件声明中也会失败。
- 示例 manifest 覆盖 Counter、LoginForm、VirtualList；当前 runtime 仍不会自动生成它，
  custom fixture schema 只在 registry 缺少具体 schema 时报告 fixture_schema_unavailable。

## 验证

- 场景单元测试验证合法 manifest 的 reset/ready 匹配。
- 缺少 manifest 仍只报告 registry_unavailable，不把未知组件当作已检查。
- workspace tests、clippy、fmt 和设计文档检查通过。

## 未覆盖

组件注册 API、manifest 编译产出、preview 启动、独立数据目录、scenario_ready 和
reset_generation 属于 S02 后续切片。
