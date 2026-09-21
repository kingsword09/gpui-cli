# 配置与响应草案

这些文件是路线设计的可解析示例，不是当前 CLI 的配置教程。所涉及的 observe/preview/check/matrix/perf/repro/upgrade 命令尚未实现；不能复制文件后就期待现有程序识别它们。

| 文件 | 用途 | 实现任务/契约 |
| --- | --- | --- |
| [observation-v2.json](observation-v2.json) | 等待型观察成功 envelope，版本/帧/产物关联 | F02/O04；[观察协议](../design/observation-protocol.md) |
| [scenarios.toml](scenarios.toml) | Counter、LoginForm、VirtualList 三个场景 | S01/S04；[场景设计](../design/scenarios-and-checks.md) |
| [fixtures/counter-zero.json](fixtures/counter-zero.json) | Counter 初态 | F01/S02 |
| [fixtures/login-invalid.json](fixtures/login-invalid.json) | 固定表单错误与允许的业务日志 | F01/S02 |
| [fixtures/list-1000.json](fixtures/list-1000.json) | 确定性生成 1000 个稳定 key 的项目 | F01/S02 |
| [matrix.toml](matrix.toml) | macOS/iOS/Android 必需，Windows 可选 | M02；[平台设计](../design/platform-matrix-and-repro.md) |
| [performance-budget.toml](performance-budget.toml) | 独立绝对/相对预算和配对采样 | G02；[性能设计](../design/performance-and-gpu.md) |
| [repro-manifest.json](repro-manifest.json) | 无源码的诊断包索引、隐私与重放前置条件 | M05；[复现设计](../design/platform-matrix-and-repro.md) |
| [template-manifest.json](template-manifest.json) | 模板基线文件、依赖组合与逻辑组 | T02；[升级设计](../design/toolchain-and-upgrades.md) |
| [doctor-v2.json](doctor-v2.json) | 按目标判定 required/optional 的结构化报告 | T01；[doctor 设计](../design/toolchain-and-upgrades.md) |

## 1. 哪些值只是占位

JSON 中 session/run/window/operation/artifact ID、产物长度、时间、工具版本和64位重复十六进制 hash 都是演示值，不来自真实执行。`0.1.0-draft` 和 `agent-native-v1-draft` 不是已发布 runtime/模板版本。只有 base commit 和当前 GPUI 依赖 revision 引用了实际代码基线，仍不代表这些草案已实现。

这里不附实际 PNG、语义树或 ZIP；repro/template 的文件清单用来展示结构，不是可通过真实文件完整性验证的归档/升级清单。实现时必须枚举全部受管理文件、计算真实 hash、验证原始内容可取得，禁止把示例 metadata 当有效证据。

matrix 中的 `REPLACE_WITH_SIMULATOR_UDID`、`emulator-5554`、`windows-lab-1` 必须在使用时替换/登记。示例不会自动启动、创建、下载任何设备。Windows 是未来远程 runner 的可选 cell，不代表本机 macOS 可以运行 Windows 应用。

## 2. 放进生成项目后如何解释

未来实现后，将 scenarios.toml 作为项目根 `gpui.scenarios.toml`，将 fixtures/ 放在相对它的 fixtures/ 目录；也可以放到 dev/fixtures/，但需同步改路径。路径始终相对定义文件再限制到项目允许根。

应用还必须注册名为 Counter、LoginForm、VirtualList 的组件、fixture schema、ready/reset、语义 ID 和环境 adapter。CLI 不会从组件名字自动生成业务 UI。这些 JSON fixture 是示例应用的数据契约，不是所有 GPUI 应用都必须接受的内置格式。

List fixture 的 count/prefix/digits 表示由组件的确定性 fixture factory 生成 item-0000 至 item-0999；不需要写1000行相同结构。条目导出 logical_id 为 `list.item.<stable-key>`，滚动后测试 `list.item.item-0042`，禁止用可变数组下标冒充稳定业务 key。

## 3. 关键语义

- API JSON schema=2、app proto=2，与场景/matrix/performance/repro/template 文件 schema=1 是不同版本维度。
- observation 是完成的观察，不是“输入后的断言已通过”；示例 check.status=not_run。presented_frame_id=null 明确表示未证明屏幕呈现。
- scenario 的 requires 使用 screenshot/semantics 别名及明确能力；input/bounds 缺失会阻止对应步骤，不能仅有图片就假设可点击。
- 每次动作由执行器重新建立/确认观察，TOML 不硬编码过期 observation_id 或可重复点击 request_id。
- LoginForm 中的文本仅为测试数据。允许 invalid_credentials 日志不表示可以忽略其他 runtime error 或 panic。
- perf 的数字只是初始预算例子，不是已测性能。没有实际环境/配对证据时相对规则应 inconclusive。
- 无源码 repro 默认 requires_source；显式提供可信且 hash匹配的源码之前，inspect 不执行程序。

## 4. 本次可以实际执行的检查

从本仓库根运行：

```bash
cargo x check-design-docs
git diff --check
```

校验器通过仓库的 Rust `x` 任务运行器执行，检查本路线文档的相对链接、任务/验收编号、依赖无环、JSON/TOML语法，以及示例间的基础引用和约束。它不是产品 schema validator，也不验证示例 hash 对应真实产物，更不证明任何 UI/GPU 能力通过。

首个实现 PR 必须把这些示例移入/复制为正式 Rust 反序列化与契约测试输入，并加入非法字段和边界变体；实现规范改变时同步更新设计、示例与验收用例。
