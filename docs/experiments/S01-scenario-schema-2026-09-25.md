# S01：场景 schema 与静态校验（2026-09-25）

状态：静态校验切片已实现；不启动应用，不执行 preview/check。

## 实现范围

- 新增 `gpui scenario validate --file gpui.scenarios.toml [--json]`。
- schema v1 使用 `deny_unknown_fields`，覆盖场景/step ID、场景数量、组件、tags、默认
  requires、theme/locale/random_seed/clock、RFC 3339 `clock_at`、viewport 和 1–200 steps。
- selector 只接受唯一 `logical_id`，或成对的 `role` + `name`；禁止 CSS/XPath 和隐式多选。
- 校验 click/type_text/key/scroll/wait_for/assert/capture 的参数、断言 expected 类型、
  baseline_id、step timeout 和有界 capability 名称。
- fixture 路径相对场景文件解析，并拒绝绝对路径、父目录逃逸、超大文件和组件不匹配；
  使用现有严格 fixture parser 计算 `fixture_hash`，默认值归一化后计算 `scenario_hash`。
- `.gpui/registry-manifest.json` 存在时校验组件名；内置三种 fixture 使用严格 parser，
  自定义组件/fixture schema 仍报告 `fixture_schema_unavailable`。registry 缺失时输出
  `registry_unavailable` warning，不把 registry 缺失转换成成功检查。

## 验证

- 合法 `docs/examples/scenarios.toml` 返回 `valid=true`、三个 fixture hash 和稳定
  scenario hash。
- 非法 selector、fixed clock 缺少 `clock_at`、未知字段、越界 fixture 与重复 ID 有定位
  错误；JSON 报告和人类输出来自同一模型。
- workspace 场景单元测试、fmt、clippy 和设计文档检查通过。

## 未覆盖

没有 registry 生成、preview/reset、真实 runtime `scenario_ready`、动作执行或 check
报告；这些属于 S02–S04，不能因为静态文件有效就报告场景通过。
