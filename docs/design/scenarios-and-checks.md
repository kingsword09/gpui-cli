# 设计契约：组件场景、输入与可重复检查

状态：拟议，未实现。任务：S01–S05、O05/O06、Q01。依赖：[观察协议](observation-protocol.md)。

## 1. 场景是预览、测试和性能测量的共同输入

一个场景描述“用哪些数据、环境和动作，观察哪个组件/界面以及什么结果”。场景必须在全新运行中建立自己的初态，不能依赖开发者上一次点击留下的状态。

以下三种运行共用场景定义：

- preview：持续显示场景，允许开发者或 Agent 交互；结果默认不是可重复测试证据。
- check：从显式 reset 开始执行有界动作和断言，生成结构化结果。
- perf：使用相同初态与动作，在性能构建中测量分布，不能直接复用 debug 数值作性能基线。

场景不是任意脚本执行器。MVP 不提供 JavaScript、shell、无限循环或任意表达式求值。

## 2. 项目组织与运行入口

拟议目录位于生成的应用项目：

```text
gpui.scenarios.toml
dev/
  fixtures/
    counter-zero.json
    login-invalid.json
    list-1000.json
  baselines/
    macos-metal/
    android-vulkan/
crates/app/src/
  previews.rs                 # debug/preview feature 下的显式注册
```

`gpui.scenarios.toml` 默认从项目根加载；`--file` 可指定其他项目内文件。路径相对于定义文件解析，再验证没有逃出项目根。发布时应排除不需发货的 fixtures/baselines，开发支持库和场景注册通过显式 feature 控制。

预览首次需要构建支持该组件的宿主。它节省的是业务初始化、导航和重复准备场景的成本；不承诺任意 Rust 编辑免编译。

## 3. 场景 schema v1

完整可解析例子见 [scenarios.toml](../examples/scenarios.toml)。schema v1 在尚未实现时是草案，首个实现 PR 必须增加正式反序列化类型、错误位置和 schema fixture 测试。

| 字段 | 必需/默认 | 语义与限制 |
| --- | --- | --- |
| `schema_version` | 必需，1 | 场景文件格式版本，独立于控制 API v2 |
| `scenarios[].id` | 必需 | 项目内唯一、小写字母/数字/点/横线 |
| `component` | 必需 | 显式注册名；不是可执行 Rust 源码字符串 |
| `fixture` | 必需 | 项目内 JSON 路径，使用内容哈希关联结果 |
| `tags` | 默认空 | 仅作测试选择，不改变结果 |
| `timeout_ms` | 默认 30000，上限 120000 | 构建完成后的场景执行 deadline，含 reset/ready/steps/观察 |
| `requires` | 默认 screenshot、semantics、scenario.reset | 能力缺失时 required 场景为 unavailable |
| `theme` | 默认 light | light/dark；通过应用适配器设置并回报实际值 |
| `locale` | 默认 en-US | 必须使用 fixture 支持的 locale |
| `random_seed` | 可选整数 | 只有应用注册了随机源适配时才保证生效 |
| `clock` | 默认 real | real/fixed；fixed 需要业务时钟注入 |
| `clock_at` | fixed 时必需 | RFC 3339 时间；real 时禁止填写，不冻结 OS/GPU 时钟 |
| `viewport.width/height` | 必需 | 逻辑像素，正整数且每边 ≤ 8192 |
| `viewport.scale` | 可选 | 请求值，真实 DPI 需由 runner/runtime 回报 |
| `ready_id` | 可选 | 显式语义节点/应用 ready 条件，不能由截图猜测 |
| `steps` | 必需，1–200 项 | 顺序动作/断言；每项有唯一 step id |

不认识的配置字段必须报错，不能静默忽略一个拼错的断言。场景 ID、step ID 重复、fixture 不存在、未知组件、总 timeout 不合理在启动应用前失败。

版本 v1 的 step 类型：`click`、`type_text`、`key`、`scroll`、`wait_for`、`assert`、`capture`。动作只支持文档定义的参数。后续新增复合动作必须 bump/扩展 schema 并定义兼容策略。

### 3.1 Step 参数契约

每一步必需 `id` 和 `type`。`selector` 是对象，只能选一种形式：`{logical_id = "counter.increment"}`，或 `{role = "button", name = "Increment"}`；不支持任意 CSS/XPath。除 capture、no_runtime_errors 和 screenshot_matches 外，动作/断言必须有 selector。坐标输入是交互 CLI 的显式高级参数，不进入首版可移植场景文件。

| type | 参数 | 完成语义 |
| --- | --- | --- |
| click | selector；button 默认 primary | 正常命中与分发完成，随后重新观察 |
| type_text | selector、text；mode 默认 replace，可为 append | 向可编辑控件发送正常文本输入，不直接设置业务对象 |
| key | selector、key（首版 Enter/Tab/Escape/Backspace） | 明确焦点后分发；键盘布局限制由 provider 回报 |
| scroll | selector、delta_x/delta_y（逻辑像素）、duration_ms（默认0） | 在指定容器按单调时间分发滚动；duration>0时分段执行，之后重新查询 |
| wait_for | selector、assertion、需要时 expected、timeout_ms | 有界重复观察，timeout 不超过场景剩余时间 |
| assert | assertion、需要时 selector/expected | 对当前有效观察判定，不隐式重试 |
| capture | label、require（默认 screenshot） | 保存该步骤的观察和产物引用 |

`text_equals` 的 expected 必须是字符串，`value_equals` 是 fixture/schema 所声明的标量类型；exists/absent/enabled/focused/visible/not_clipped/no_runtime_errors 不接受 expected。`screenshot_matches` 使用 baseline_id，不接受任意文件路径。每种类型的无关字段也按未知字段拒绝。步骤自己的 timeout 默认受场景总 deadline 约束，不能相加后超出它。

scroll 的 duration_ms 必须非负且不超过剩余 deadline，实际输入次数/间隔由 provider 回报并进入性能环境键；不能声称每个平台都准确产生相同帧数。分段事件的总位移仍等于请求 delta，取消/帧丢失不能触发整段自动重放。

## 4. 应用侧注册与 reset

### 4.1 接口草图

下列是要实现的 API 形状，不是 GPUI 或当前 CLI 已有的可编译示例：

```rust,ignore
register_preview(PreviewDescriptor {
    name: "Counter",
    fixture_schema: counter_fixture_schema(),
    create: create_counter_from_fixture,
    reset: reset_counter,
    supported_environments: counter_environments(),
});
```

注册项需要提供：组件名称/版本、fixture schema、创建入口、reset 方法、显式 ready 状态、支持的主题/语言/时钟/随机适配。运行时枚举这些元数据给 CLI；不能通过反射假设任意 `Render` 类型都能自动构造。

### 4.2 初态协议

1. runner 为 check 创建独立运行、独立数据目录；profile 采用相同隔离规则。
2. runtime 验证 fixture/schema/哈希，创建组件并应用环境。
3. 初始化完成后发 `scenario_ready`，包含 scenario_id、fixture_hash、实际环境、reset_generation。
4. `ready_id` 存在时，等待其可查询状态满足条件；随后建立初始 observation。
5. 每次 retry 重新创建或 reset，必须增加 reset_generation；不能在失败后的半成品状态接着执行。

MVP 默认使用新进程获得最强隔离。进程内 reset 是可选优化，只有证明取消旧异步任务、订阅、计时器和资源引用后才能声明支持。

现有 Live state snapshot 默认不用于 check/perf。用户需要以某份状态开始测试时，将筛选后的状态写成显式 fixture，而不是自动导入 `.gpui/sessions/` 最新文件。

### 4.3 非确定性边界

- clock=fixed 只控制应用注入的业务时钟，不自动冻结 OS 动画和 GPU 时钟。
- random_seed 只控制应用注入的随机源；未接入的随机因素列入 `uncontrolled_inputs`。
- 网络采用 fixture adapter 或测试服务；未提供适配时记录真实网络，不宣布离线确定性。
- 字体、locale、DPI、主题和 backend 记录实际值；请求值与实际值不同会使严格视觉比较不可比。

## 5. 预览生命周期

拟议命令：

```bash
gpui preview Counter --scenario counter-basic --target desktop --json
gpui preview Counter --scenario counter-basic --target ios --sim "iPhone 17 Pro@26.2"
gpui dev observe --sync --window w1 --require screenshot,semantics --json
```

preview 由一个会话持有，沿用 Live 构建、错误保留与重启能力。首次初始化和源码重启后均从场景 fixture 恢复；交互开发模式可以显式启用 state snapshot，但输出必须标记 `interactive_state:true`。

一个 preview 宿主可以登记多个窗口，但不能在未选择窗口时把任意一个截图当成目标组件。组件重新注册/签名改变需要重建；fixture/主题变化可以在支持 reset 的 runtime 中重新创建场景。

## 6. 节点选择和动作前置条件

动作必须绑定 observation_id、run_id、window_id 和 expected revision。推荐 logical_id；role/name 作为明确的辅助选择器。

| 情况 | 结果 |
| --- | --- |
| 0 个匹配 | selector_not_found |
| 多个匹配 | ambiguous_selector；返回有限候选 |
| 节点 disabled | element_disabled |
| 节点不在可视范围 | element_not_visible；调用者显式 scroll 后重新观察 |
| 被其他元素遮挡 | element_obscured；不得直接调用业务 handler |
| 控件已被替换/run 已变化 | stale_observation |
| 只有坐标 | 必须绑定截图尺寸、方向、DPI 转换和观察身份 |

输入通过 GPUI 正常事件分发和命中测试，在 UI 线程执行。系统键盘、IME、权限弹窗、文件选择器通过平台适配器处理；runtime 不声称自己模拟了完整 OS 输入路径。

MVP 不支持一个表达式同时选择“多个按钮并批量点击”。虚拟列表项必须先滚动使其进入可查询范围；业务稳定 key 不能被列表索引替代。

## 7. 输入请求与幂等

拟议命令：

```bash
gpui dev act click --id counter.increment --observation o-7 --request-id click-1 --json
gpui dev act type-text --id login.password --text "bad-password" --observation o-8 --request-id type-1 --json
```

`act` 复用 v2 operation 模型。CLI 未指定 request_id 时生成随机 ID，所有连接重试复用原 ID；客户端不得为同一逻辑操作的网络重试生成新 ID。

- `(session_id, run_id, request_id)` 是去重键，保存规范化参数哈希和结果。
- 相同 ID、不同参数返回 idempotency_conflict。
- 已完成请求返回保存结果，不再次点击。结果缓存上限每 run 1000 项/8MiB/10 min；此外保留 run 级请求墓碑（request_id、参数哈希、接受/投递标志），最多 10,000 项或4MiB（先到者为上限）。结果淘汰后相同 ID 返回 outcome_unknown，不猜测没执行过；不同参数仍返回 idempotency_conflict。
- 墓碑满时拒绝新的动作 ID 并返回 quota_exceeded，不淘汰已有 ID 来腾位置；调用方必须建立新 run。输入投递前先持久记录接受状态，恢复后不能证明结果的 ID 为 unknown。旧 run 的请求永远不能转发到新 run；迟到重试为 stale_observation / target_exited。
- 输入已入 UI 队列但连接中断时，不能立即宣称 cancelled。先查状态；run 崩溃后无法确认的结果为 unknown。
- 同一窗口一次只执行一个动作；多步场景获得窗口执行租约，阻止另一个客户端插入操作。
- 用户在应用窗口直接交互时，check 检测到非本场景输入应标记 contaminated/inconclusive；不覆盖用户输入或强抢控制。

这里承诺拒绝重复投递与显式 unknown，不承诺跨崩溃的 exactly-once 业务事务。结果缓存、持久墓碑、runtime投递边界分别测试；无法确认“已经接受但未执行”的请求不得自动再发一次。

`action.finished` 只代表分发完成；必须另做 observation/断言才能确认业务结果。每次动作后若 UI 改变，之前 node_ref 失效。

## 8. 断言模型

初始断言种类：

| assertion | 比较对象 | 限制 |
| --- | --- | --- |
| exists / absent | 稳定节点选择器 | 缺 semantics 不能当 absent |
| value_equals / text_equals | 已知类型的值/文本 | 不使用隐式正则或 locale 猜测 |
| enabled / focused | 语义状态 | 未知字段为 inconclusive |
| visible / not_clipped | bounds 与 clip 链 | 需要 semantics.bounds；不凭截图猜边界 |
| no_runtime_errors | 指定操作 seq 范围的新增错误 | 历史错误仍保留，不能因本轮通过删除 |
| screenshot_matches | 同平台、同配置视觉基线 | 见平台设计的容差与批准流程 |

`wait_for` 使用带 deadline 的条件轮询/事件订阅，默认每次观察无需重编译；不采用固定 sleep 作为“已就绪”的证据。

断言结果：passed / failed / inconclusive。缺少必需能力、受污染环境、无法定位版本均不能算 passed。场景的所有必需断言 passed 且无 unknown 动作时，场景才 passed。

用户预期的业务校验错误可以在 fixture 中声明允许的错误代码，不能以“本轮所有日志必须没有 error”替代业务语义。异常豁免要求精确代码/来源/阶段和测试说明，禁止全量忽略。

## 9. 检查执行器

拟议规范入口为根命令 `gpui check`；旧 Agent 设计中的 `gpui dev check` 只是未实现的早期示意，不作为必须支持的别名。

```bash
gpui check --scenario counter-basic --target desktop --json
gpui check --scenario login-invalid --matrix gpui.matrix.toml --json
```

执行步骤：解析与静态验证 → 获得 runner/窗口租约 → 构建匹配源码 → 新运行/reset → ready → 初始观察 → 逐项执行动作/断言 → 最终观察 → 保存结果 → 清理测试运行并释放租约。

正式 check 使用冻结源码输入，不能把交互 Live 的 tracked_scan 结果自动视为严格测试。单目标 job 的拟议 `--timeout` 默认 10 min，包含租约等待/构建/安装/执行/清理；场景的 timeout_ms 从进入 reset/ready 执行阶段开始，实际 deadline 取它与 job 剩余时间的较小者。matrix 使用每个 target 的总 timeout，所有子操作都不能越过父 deadline。

每个 step 记录 request/operation ID、前后 observation、起止时间、输入转换、断言实际值、错误与日志 seq 范围。失败即停止后续依赖步骤；独立矩阵目标是否继续由 matrix.fail_fast 控制。

自动重试默认 0。诊断性 retry 需要新初态；即使 retry 成功，也保留首次失败并标记 flaky，CI 默认不把 flaky 当作稳定通过。

结果文件包含 scenario 文件哈希、fixture 哈希、源码快照哈希、runner 环境、实际屏幕属性、每一步结果及产物引用。人类报告和 JSON 来自同一结果模型。

## 10. 预览热参数实验 S05

允许应用显式注册有限的可调参数，例如颜色、间距、圆角、字体大小和测试数据量。每个参数声明类型、合法范围、作用域与是否需要重新布局。

- 参数值通过结构化消息传递，不修改任意 Rust 对象内存，不执行字符串代码。
- 一次变更形成独立 overlay_revision；观察记录源码版本和 overlay 版本。
- 撤销、重启、关闭 preview 默认清除 overlay。
- “固化为源码”必须先生成可审阅 patch，校验源文件哈希后才应用；不能把运行时试验结果当成已提交实现。
- 不承诺任意 GPUI builder 调用都能转换成参数；没有注册的属性只读或 unavailable。

S05 是实验支线，不阻塞 G2。只有典型设计迭代确实比重建更快且不会造成源码/预览混淆时才推广。

## 11. 标准夹具与模块落点

| 夹具 | 必须覆盖 |
| --- | --- |
| Counter | 初始值、点击、disabled、重复 request、重启后节点失效 |
| LoginForm | 空输入、无效密码、请求中、成功、取消、键盘遮挡 |
| VirtualList | 1000 项、滚动定位、稳定 key、大字号、资源延迟 |

拟议 `src/scenario/` 拆为 schema、registry、executor、assertions、report。只有 runtime adapter 接触 GPUI 类型；纯计划解析和结果比较在无 GPU CI 中可测试。

验收见 [S-01–S-10](../roadmap/acceptance-matrix.md)。G2 至少需要 Counter 和 LoginForm 在真实 macOS 窗口中完成受控执行，以及 VirtualList 的定位/裁剪验证；纯 mock 路由测试不替代真实输入路径。
