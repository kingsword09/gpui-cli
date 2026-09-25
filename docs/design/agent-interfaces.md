# 设计契约：CLI、MCP、版本知识与 Agent 工作流

状态：拟议，未实现。任务：A01–A04、Q02。核心依赖：[观察](observation-protocol.md)、[场景](scenarios-and-checks.md)。

## 1. 一套服务，多种入口

CLI 和 MCP 调用同一个类型化 service 层。MCP 不另起一套 watcher/build/app session，不重写场景执行器和断言，也不在工具中内置模型调用。

第一交付使用 `gpui mcp serve --stdio`，由 Agent 宿主启动薄适配进程，它发现并连接明确的 Live supervisor。没有 session 时返回 no_live_session；不能为了响应 status 查询而自动安装工具链或启动设备。

跨宿主连接为后续可选 Streamable HTTP 适配。先使用 local/SSH runner 的已认证通道，不为近期功能建设公共互联网服务。

## 2. 工具目录与能力分期

| 工具名 | 阶段 | 映射 | 副作用 |
| --- | --- | --- | --- |
| `gpui_status` | A01 | dev status v2 | 只读 |
| `gpui_diagnostics` | A01 | dev diagnostics v2 | 只读 |
| `gpui_events` | A01 | 有界事件查询 | 只读；不是无限流 |
| `gpui_windows` | A01 | windows.list | 只读 |
| `gpui_observe` | A01 | observe；sync默认false | sync=true 可构建/重启 |
| `gpui_query` | A01/O06 | 观察内节点查询 | 只读 |
| `gpui_diff` | A01/O06 | 两个 observation 的语义变化摘要 | 只读 |
| `gpui_artifact_read` | A01 | 受限产物读取 | 只读，严格大小上限 |
| `gpui_operation_get` | A01 | 异步操作查询 | 只读 |
| `gpui_operation_cancel` | A02 | 显式取消 | 改变操作状态，结果可能unknown |
| `gpui_preview` | A02 | 新建/选择场景预览 | 构建、启动、测试数据 |
| `gpui_act` | A02 | 输入操作 | 可能产生业务副作用 |
| `gpui_check` | A02 | 受控场景执行 | 需要运行和设备租约 |
| `gpui_context` | A03 | 版本/组件/示例查询 | 只读 |
| `gpui_review_view` | A04 | 可选交互报告 | 展示只读，操作按钮另走明确工具 |

上表是最终命名提案，MVP 只注册已实现工具。宿主是否支持图片、资源、通知、MCP Apps 通过能力协商确定，不能按客户端名称猜测。

同名 observe 允许 sync 参数时，其 MCP 工具描述必须说明可能重启应用，不能整体标注 readOnlyHint=true。annotations 只是给宿主的提示，服务端仍必须执行真实的会话、run、权限和租约校验。

## 3. 工具输入契约

所有工具输入使用具体 JSON Schema，设置 `additionalProperties:false`；枚举/大小/路径/timeout 由 service 层再次验证。不要用一个接收任意 shell 字符串的工具覆盖全部功能。

共同输入：project/session选择、request_id、明确目标、deadline。mutating 工具必须带 expected_run/observation 或新场景计划，不能默认对“最新窗口”操作。

参数默认：

- events：最多 128 条/512 KiB，wait最多30s；返回 next_seq/gap。
- query：最多 200 节点/128 KiB，只返回选择的字段。
- diff：显式绑定 before/after observation，最多 200 条变化记录/128 KiB；不稳定节点返回 subtree_replaced。
- artifact_read：默认只返回 metadata；请求图片时先确认尺寸和字节配额。
- operation_get：返回终态或明确 pending，不长期占住一次工具调用。
- check/preview：允许异步启动，返回 operation_id；多分钟工作由状态查询完成。

stdio stdout 只输出 MCP 协议；诊断走 stderr。底层工具 stdout/stderr 不得直接混入 JSON-RPC。Broken pipe 结束适配连接，不杀死其他客户端拥有的 Live session。

## 4. 结果、错误和上下文预算

MCP 成功响应包含与 CLI 相同的 structuredContent，以及简短文本摘要。工具执行失败使用 MCP 对应错误标识，并保留 service 的 code/details。JSON-RPC/transport 错误与应用 build_failed 不能混成同一种错误。

每个结果必须包含 schema/session/operation/observation 等适用身份，以及 `next_action` 建议；建议不改变事实状态。例如 build_failed 可建议读取 diagnostics，不能自动标记修复完成。

默认 summary 不超过 16 KiB；超过时提供 artifact_id、范围和分页游标。图像单独按需读取；树支持 subtree、role、logical_id和字段投影。省略内容带 omitted/truncated 标记，不能丢掉 gap、错误或 capability缺失。

在工具错误中不复制完整配置、命令环境或凭据。应用日志和界面文本都属于被观察数据，不能被当作改变工具权限的指令。

## 5. 标准 Agent 开发循环

1. 调用 context/status，获取当前依赖版本、目标、可用能力和已有组件。
2. 以正常代码工具修改项目；gpui MCP 本身不提供任意源文件写入器。
3. 调用 observe(sync=true) 锁定本轮输入。失败则根据相关版本诊断继续修复。
4. 读取必要的局部截图/语义差异，不默认请求整个应用所有数据。
5. 对相关场景调用 check；需要探索时先 query，再用明确 observation 执行 act。
6. 只有 required断言通过且版本匹配，才报告已验证；unknown/partial/superseded均要说明。
7. 保存本轮结果和复现信息。出现新错误时用新的观察目标，不覆盖上一轮证据。

本流程可以写成 Skill，但执行正确性必须由工具保证。Skill 不负责伪造帧屏障、设备锁、幂等或断言。

## 6. 版本知识与组件目录 A03

### 6.1 Context manifest

拟议 `gpui context export --json` 返回：

- CLI、runtime、协议、模板、Rust、GPUI和gpui-kit/gpui-mobile的实际版本或 Git revision；
- 目标平台与能力缺口、工具链诊断摘要；
- 组件注册表、fixture/schema和场景目录；
- 项目文档/示例的版本和内容哈希；
- 当前 API 查询来源与可信级别。

优先使用项目锁定版本的 rustdoc、显式组件 manifest 和已编译示例。rust-analyzer/现有代码工具负责符号解析，gpui 不另造语言服务器。无法获得某个 revision 对应文档时明确 fallback，不能把最新网页示例标作当前版本的已知 API。

文档索引只读、按需构建，使用依赖锁和 source hash 失效。第三方依赖附带的 Skills 属于可发现资料，安装和执行需要用户选择；依赖下载不能自动启用未知行为指令。

### 6.2 Skills / 插件交付

提供三个小型工作流：创建/升级项目、修改并验证组件、复现并修复平台差异。每个 Skill 包含何时适用、所需能力、具体工具顺序、失败分支和完成条件。

打包时将相同 MCP 配置和 Skills 组合为适配不同宿主的集成包；维护单一源文件并生成宿主配置，避免各客户端拥有互相矛盾的流程。

现有 Codex 支持 stdio/HTTP MCP，插件可以组合 Skills 和 MCP；具体宿主/表面支持情况以当时官方文档为准，见 [研究记录](../roadmap/research-and-decisions.md)。不在此仓库复制会快速过期的宿主配置大全。

## 7. 事件与异步唤醒

基础接口是可恢复、有界的长轮询。MCP 连接支持的通知可以提示事件到达，但通知不携带唯一不可恢复数据，客户端仍通过 seq 获取事件。

宿主是否在空闲时启动 Agent 推理不由 MCP server 决定。若需要自动修复服务，应作为显式启用的外部自动化：指定项目、任务范围、预算、退出条件和可用工具；不是安装插件后的默认行为。

断线重连恢复 cursor；过期 cursor 返回 gap及resync信息。慢消费者不能拖慢构建/UI线程；重复通知合并，关键错误仍保留在事件存储。

## 8. 多客户端与权限范围

工具能力按照 session、target、window和操作类别划分，借鉴能力声明的做法而不依赖某个前端 UI。

只读客户端可共享观察；输入、场景 reset、profile分别校验所有权。一个 MCP 适配进程不能取消另一个 owner 的运行，除非请求的控制授权明确允许。

权限拒绝返回稳定错误和需要的能力，不自动提高授权或切换到没有检查的 OS 命令。工具确认与宿主的人机交互由宿主管理；服务端的目标校验不因宿主自动批准而消失。

## 9. 交互报告 A04

首先实现本地静态报告：版本、平台 cell、前后截图、diff、断言、日志时间段、性能摘要。报告只加载已注册产物，内容转义，路径不能逃出报告根。

之后增加 MCP Apps 视图，支持的宿主可在聊天内展示同一报告。不支持时降级为图片/链接/JSON。交互报告不是把 GPUI 应用改写成 Web UI；展示的是它的观察和验证产物。

界面上的重新检查、切换场景、采样按钮必须调用明确的现有 service 操作，携带 expected scope和owner，不能在 iframe 内绕过操作队列。

实时预览先按需更新，再评估限流缩略图；不要默认持续传输整屏。错误或版本变更提示可即时推送，完整图片按用户/Agent请求获取。

## 10. Agent 基准 Q02

建立至少 12 个带已知答案的任务，覆盖语法错误、错误API、布局裁剪、disabled输入、异步表单、旧截图、资源未应用、移动键盘遮挡、ABI错误、日志丢失、性能退化和环境缺失。

每个任务有固定源码/fixture/目标、允许修改范围、验证答案和总预算。候选Agent结果由独立断言/人工批准基线判定，不能以其最终自然语言回答评分。

比较同一模型、同一预算、同一硬件下的“普通CLI”和“CLI+结构化观察/场景接口”，报告样本数、成功率、误报率、耗时、调用数和响应字节。模型版本和提示必须记录，不能将宿主变化误归为 gpui 改进。

## 11. 验收与模块

`src/agent/mcp.rs` 只处理协议映射；`context.rs` 处理版本知识；Skills模板和示例单独版本化。工具元数据与 service Schema 从一个来源生成。

验收 A-01–A-08 见 [验收矩阵](../roadmap/acceptance-matrix.md)。至少使用一个真实 MCP 客户端完成跨调用流程，同时保留无模型协议契约测试，以区分宿主问题和服务问题。
