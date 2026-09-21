# 实施清单：从 D1 到可验证的跨平台开发闭环

状态：全部任务为 `planned`，本文不是已实现功能列表。基线：`6d091b6`；日期：2026-09-21。

入口：[总路线](../ROADMAP-agent-native-development.md)。验收编号的完整步骤见 [验收矩阵](acceptance-matrix.md)，接口语义以各 [专项设计](../ROADMAP-agent-native-development.md) 为准。

## 1. 如何执行

每个任务是一个可审查的工作包，不要求硬塞进一个大 PR。表中的依赖是完成该工作包的硬前置；可以提前做不改变能力声明的类型/测试准备。字段中的路径是建议落点，新路径尚未存在，不应为凑目录提前创建空实现。

- `planned`：尚未开工；`in_progress`：有负责人、分支和失败用例；`in_review`：代码与证据齐全；`done`：合并且列明的验收通过。
- `blocked_by_experiment`：上游/平台实验没有通过，附实验记录和重试条件；`deferred`：明确延后，不能被统计为已交付。
- S = 0.5–1、M = 1–3、L = 3–5 个有效人日；不含等待设备、上游、签名和 review。超过 L 必须继续拆 PR，估计不是交付日期承诺。
- 优先级 `core` 是该门槛的必需内容；`optional` 必须独立声明 provider/平台支持，不阻塞核心门槛。
- 一个实现 PR 附 `task_id + acceptance_ids + evidence_root`。任务表中引用多个用例时，应说明本 PR 覆盖哪一部分；任务全部完成才可以改为 done。
- CI 从 F01 开始逐项随功能加入，不能等 Q01 最后一次性补测试。Q01 负责贯通并审计已有各层任务。

## 2. 任务与硬依赖总表

| ID | 门槛 | 优先级 | 规模 | 前置任务 | 验收 | 状态 |
| --- | --- | --- | --- | --- | --- | --- |
| F01 | G0 | core | M | — | P-01, P-02 | in_review |
| T01 | G0 | core | M | — | T-01, T-02, T-03 | in_review |
| P01 | G0 | core | L | F01, T01 | O-05, O-10, O-11, P-03 | in_review |
| T02 | G1 | core | M | F01 | T-04 | in_review |
| F02 | G1 | core | L | P01, T02 | C-01, C-02, C-03, C-04, C-05 | planned |
| T03 | G1 | core | M | T02 | T-04, T-05 | in_review |
| T04 | G1 | core | L | T03, F02 | T-06, T-07, C-02 | planned |
| O01 | G1 | core | M | F02, P01 | O-04, O-05 | planned |
| O02 | G1 | core | M | F02 | O-06, O-07 | planned |
| O03 | G1 | core | M | F02 | O-08, O-12, C-05 | planned |
| O04 | G1 | core | L | O01, O02, O03 | O-01, O-02, O-03, O-04, O-08, O-09, O-10 | planned |
| O05 | G2 | core | L | P01, F02, O01 | O-10, O-11 | planned |
| O06 | G2 | core | M | O05, O03 | O-11, A-03 | planned |
| S01 | G2 | core | M | F02 | S-01, S-09 | planned |
| S02 | G2 | core | L | S01, O04 | S-02, S-09 | planned |
| S03 | G2 | core | L | O06, O04 | S-03, S-04, S-05, S-06, S-08 | planned |
| M03 | G2 | core | M | T01, F02 | M-04, M-05, M-06 | planned |
| M04 | G2 | core | L | T01, F01 | M-07, M-08 | planned |
| S04 | G2 | core | L | S01, S02, S03, M04 | S-02, S-03, S-07, S-08, S-09, R-05 | planned |
| A01 | G2 | core | M | O04, O03 | A-01, A-02, A-03 | planned |
| A02 | G2 | core | M | A01, S04, M03 | A-01, A-04 | planned |
| A03 | G2 | core | M | T02, S01, F02 | A-05 | planned |
| S05 | G2 | optional | M | S02, S04 | S-10 | planned |
| M01 | G3 | core | L | O04, T01, M03 | M-01, M-02, O-08, O-10 | planned |
| M02 | G3 | core | L | S04, M01, M03, M04 | M-03, M-06, M-08, R-05 | planned |
| M05 | G3 | core | L | M02, O03 | R-01, R-02, R-03, R-04, R-05 | planned |
| Q01 | G3 | core | M | F01, O04, S04, M01 | C-02, O-01, O-11, S-03, M-01, M-02, R-04 | planned |
| G01 | G4 | core | L | F01, P01, O01 | P-03, P-04 | planned |
| G02 | G4 | core | L | G01, S04, M04, M03 | P-05, P-06 | planned |
| T05 | G4 | core | M | F01, M04 | T-08, T-09, P-02 | planned |
| T06 | G4 | core | L | T05, M04 | T-10, M-08, P-02 | planned |
| Q02 | G4 | core | M | F01, A02, M02 | A-07, A-08 | planned |
| G03 | G4 | optional | L | G01, M03, O03, S04 | P-07, P-08 | planned |
| M06 | G4 | optional | L | M02, M03, M04, M05 | M-09, M-10 | planned |
| A04 | G4 | optional | M | A01, O03, M02 | A-06, A-04 | planned |

依赖补充：A01 的基础 MCP 不依赖语义导出；只有 O06 完成后才注册 gpui_query。G01 标在 G4，但它的 hook/开销实验在 G0/G1 就可以准备。M04 前移到 G2，因为正式 check 的确定性不能依赖可变工作目录；M01 依赖 M03，因为移动安装/启动不得绕过设备租约。

## 3. 可直接采用的实施批次

| 批次 | 工作包 | 批次出口 |
| --- | --- | --- |
| 0 | F01、T01 | 可重跑的基线、三种夹具定义、目标环境报告 |
| 1 | P01、T02；T02 后可做 T03 | macOS 可行性结论、可追溯模板基线 |
| 2 | F02；之后 O01/O02/O03、T04 | v1 无回归，v2/runtime 版本、窗口/ACK/产物可独立测试 |
| 3 | O04 | G1 观察闭环；真实窗口验收与旧项目迁移说明 |
| 4 | O05/O06、S01/S02、M03/M04、A01/A03 | 语义/场景前置能力、冻结输入、设备隔离 |
| 5 | S03 → S04 → A02 | G2 受控交互与 Agent 调用闭环 |
| 6 | M01 → M02 → M05；Q01 收口 | G3 本地跨端验证与复现 |
| 7 | G01 → G02、T05 → T06、Q02 | G4 固定环境的性能/效率证据 |
| 支线 | S05、G03、M06、A04 | 各实验通过后单独晋升，不改变核心门槛 |

同一批次表示可以分别推进，并非所有列出的任务都可以无视依赖并行。单人执行时按照总表的拓扑顺序逐项推进；不需要先组建多 Agent 调度系统。

## 4. G0：基线与可行性

### F01 · 基线指标与固定夹具

代码落点：现有 `src/commands/live.rs`、`src/devserver/session.rs`、`output.rs`、`tests/`；拟议 `tests/fixtures/`、`src/devserver/timing.rs`。

1. 保存基线 CLI/v1 模板测试材料及 commit/工具链摘要；大二进制存 CI artifact，不提交进仓库。
2. 建立 Counter、LoginForm、VirtualList 的小项目及 fixture 约定；阶段一可以先接入普通窗口，场景注册由 S02 补齐。每个夹具说明正常值和可注入错误。
3. 给输入扫描、Cargo、原生打包、安装、启动建立单调 span；后续未实现阶段记 not_instrumented，不能填 0。
4. 基准驱动器保存固定小修改/构建错误/资源修改，执行 10 次预热和 30 次测量；冷/热缓存、失败样本分组。

PR 拆分：夹具/基线契约 → span 和有界记录 → 重跑脚本及报告。输出 `environment.json`、`spans.ndjson`、`summary.json` 和原命令。

验收：P-01/P-02；功能对照运行现有 fmt/clippy/test。回退：关闭新增计时，不改变 D1 状态语义。不能先承诺“提速百分比”再选择样本。

当前代码交付了严格 fixture 契约、supervisor 单调 span、有限 `spans.ndjson` 和可重跑
的 headless 基线驱动器；本地完整运行已覆盖 3 个夹具、每个 10 次预热和 30 次测量，
并保留编译失败/恢复样本。真实 native 安装失败以及 T05/T06 的索引/缓存对照仍需在
对应平台/任务完成后补齐，不能将这次 headless 结果写成跨平台性能结论。

### T01 · Target-aware doctor

代码落点：`src/commands/doctor.rs`、`src/config.rs`、`src/device/inventory.rs`；拟议 `src/toolchain/{probe,report,requirements}.rs`。

1. 抽取按目标解析的 requirements，不再把所有平台工具都作为 host 的硬要求。
2. 将工具探测统一成有 deadline 的子进程执行，记录退出码/版本/错误摘要；path 存在、非零退出、超时是不同结果。
3. 从项目 Gradle/AGP、目标 ABI、Rust minimum 生成预期；Android serial 和 iOS UDID 解析复用现有设备目录。
4. 新增 JSON schema v2、明确 required/optional、建议命令 argv；普通输出由同一模型渲染。

PR 拆分：纯报告/规则测试 → 实际 probes/超时 → CLI 和真实 SDK 验证。验收 T-01/T-02/T-03。输出 `doctor.json`、探测命令与 host matrix。

回退：保持原人类命令可用，去掉有问题的 probe 并标 unknown。禁止顺手安装 SDK、接受许可或修改签名。

当前代码交付了 schema-v2 target-aware doctor、required/optional 退出规则、bounded
command probe、项目默认 target、显式 iOS/Android 设备选择和 JSON/人类共用报告模型。
完整 SDK/JDK/AGP/build-tools 组合、真实多设备矩阵和敏感日志筛除仍需平台验收后再改为
`done`。

### P01 · macOS GPUI 观察 PoC

代码落点：独立实验夹具；记录进入拟议 `docs/experiments/P01-<date>.md`，不直接修改生产默认能力。

1. 固定 `src/template.rs` 的 GPUI family/revisions，逐项检查普通构建与 test-support 构建的依赖差异。
2. 按 [PoC 操作单](acceptance-matrix.md) 验证 scene readback、OS window capture fallback、窗口生命周期和 UI probe。
3. 验证语义激活、不依赖用户启动屏幕阅读器的导出、logical_id/role/bounds/clip；缺字段记录最小上游补丁，不编造 adapter API。
4. 测量同场景读回、编码、帧 hook 开销；确定完成 scene 与 presented frame 是否有独立证据。

输出源码 diff、依赖/feature 表、截图/树及哈希、readback 生命周期说明、adopt/limited_adopt/reject 结论。O-05/O-10/O-11/P-03 在此运行实验子项；不能因 PoC 通过就把完整服务用例标通过。

回退：scene capture 不可用但 OS 截图可验证时允许 limited_adopt/best_effort；树缺失阻塞 O05/G2，没有任何可信截图路径则阻塞 G1。外部上游等待不计入 L 的有效工时。

## 5. G1：兼容、资源和可信观察

### T02 · 模板基线 manifest

代码落点：`src/template.rs`、`src/commands/init.rs`、`templates/gitignore`；拟议模板 metadata 类型和历史基线读取器。

1. 为新生成文件计算 SHA-256、逻辑组及模板依赖组合；manifest 单独留在 `.gpui/` 且允许 Git 跟踪。
2. 记录真实可取得的 base 内容位置和发行摘要；只记录 hash 但无法取得内容不能算可升级。
3. `init --add` 更新新平台组，不将已修改的共享文件重新登记成原始基线；冲突保持现有保护。
4. 建立旧项目唯一匹配/多候选/完全自定义三种识别结果；无法识别时只输出人工迁移步骤。

验收 T-04；输出生成项目 fixture、manifest、可寻址基线包测试。回退：保留 metadata，不更改用户文件；元数据失效时拒绝自动升级。

### F02 · Protocol/runtime 抽取与双版本兼容

代码落点：`Cargo.toml`、`src/devserver/{protocol,control,events,app_channel,session}.rs`、`templates/app/src/live.rs`；拟议 `crates/gpui-dev-protocol` 和 `crates/gpui-dev-runtime`。

1. 先抽取 D1 类型/帧编解码与应用端通信，保留旧模板行为和 v1 golden fixtures。
2. 加入 v2 envelope、能力交集、request/operation 身份、双版本握手与有限 UI 队列；回复通过 ID 路由。
3. 实现一对一 v1 事件投影、根目录 v1 state/archive、独立 v2 journal；未知事件用 debug 占位日志，避免旧 reader 的假 gap。
4. 引入明确 gpui-dev/gpui-profile feature 边界；release+gpui-dev 和同时启用两种 feature 均编译报错。
5. 整理本地开发路径覆盖与发布版本依赖；按 protocol → runtime → CLI 顺序演练 package/生成项目。

PR 拆分：纯类型抽取 → runtime/D1 回归 → v2 协商/投影/状态机 → feature 与包装验证。L 是工程量初估，若抽取无法独立评审则重新估计，不能删兼容测试。

验收 C-01 至 C-05。输出三代连接矩阵、旧 CLI 实际 follow/archive 记录、release 端口/二进制检查、package 清单。回退：新能力 feature 关闭仍保留 D1；不能发布引用本机绝对路径的模板。

### T03 · 三方 upgrade plan

代码落点：`src/template.rs`、`src/commands/mod.rs`、`src/main.rs`；拟议 `src/upgrade/{baseline,plan,merge}.rs`、`src/commands/upgrade.rs`。

1. 按 B/L/N 构建只读计划，逐文件记录旧/新/当前 hash、动作、逻辑组、冲突及工具链变化。
2. 首版只自动替换未修改文件、合并有明确规则的 TOML 字段；Rust/Swift/Java 双方修改默认冲突。
3. 保留 TOML 注释和用户自定义 signing/ABI；新增文件碰撞、上游删除均有显式决策。
4. plan 输出验证命令和成本；无基线/歧义先报错，不落盘修改项目文件。计划持久化只能写独立缓存，不改变受管理文件。

验收 T-04/T-05，包含 PR #18 修复对应的历史 Android 模板夹具。输出 human diff、plan JSON、逐文件预期结果。

回退：plan 完全可丢弃；不提供“强制覆盖全部”兜底。apply 未实现时只能称迁移预览。

### T04 · Upgrade apply/recover 事务

代码落点：拟议 `src/upgrade/{transaction,journal,recover,validate}.rs`；复用现有临时文件/原子 JSON/子进程执行模式。

1. 在项目锁内重核 plan、symlink 和所有输入，先写 journal/精确备份，再准备输出。
2. 逐文件 check-and-replace，manifest 最后提交；定义 prepared/writing/validating/committed/recovery_required 状态。
3. 注入每次写入前后和验证阶段崩溃；recover 根据 hash 判断只恢复本事务仍拥有的修改。
4. 验证 missing toolchain 标 not_run；保留备份和日志直到明确清理策略允许回收。

PR 拆分：journal/恢复纯文件测试 → apply/锁/并发 → 实际旧模板迁移。验收 T-06/T-07/C-02。输出完整升级/失败恢复/用户并发编辑证据。

回退：保留 transaction 并按记录恢复，禁止宽泛 Git reset 或删除项目目录。G1 发布时同时提供迁移器或明确的受测手动迁移路径；不能假设旧项目会自动获得 runtime。

### O01 · 窗口注册与 UI heartbeat

代码落点：runtime GPUI adapter，`src/devserver/app_channel.rs`、`session.rs`；拟议 `src/devserver/windows.rs`。

1. 以 run 作用域注册/关闭窗口，记录尺寸、scale、前后台及能力变化；不使用全局自增 window ID 作为身份。
2. 实现 UI 线程短 probe，网络线程响应不能冒充 UI 响应；每窗口最多一个周期 probe。
3. 使用单调 deadline、1s 周期/3s 策略，正确处理休眠、挂起、关闭、run 切换和迟到回复。
4. 增加拟议 windows/status v2 输出，明确 unknown/suspended/unresponsive。

验收 O-04/O-05；输出真实 UI 人为阻塞和恢复记录。回退：关闭 probe 则 UI=unavailable，不能退回“进程活着即健康”。

### O02 · 资源事务、删除和 ACK

代码落点：`src/commands/live.rs`、`src/devserver/inputs.rs`、`protocol.rs`、`templates/app/src/lib.rs` 及新 runtime assets adapter。

1. 将变化组成带 manifest/hash 的事务，支持显式删除、暂存、commit、重连对账。
2. app 分别确认 received/cache_invalidated/required_loaded，错误绑定路径和 transfer_id。
3. UI 刷新/缓存失效后把资源 revision 绑定到 scene；必需资源加载失败不能标 applied。
4. 测试延迟解码、被删除图片、部分收到后断线和同路径再次修改。

验收 O-06/O-07。输出事务事件与对应新/旧图片，证明 socket 写成功不等于使用新资源。回退：旧 runtime 继续 sent-only，capability 显示 unavailable。

### O03 · 产物库与有界分块

代码落点：拟议 `src/devserver/artifacts.rs`、协议 transfer 类型、runtime transport；复用 `events.rs` 的有界/原子写模式。

1. 定义 artifact/transfer manifest 与 declared→published 状态，128KiB chunk、offset、声明大小和 hash。
2. 新增按 artifact_id 读取，不暴露任意宿主路径；验证图片尺寸、树大小、并发数和 session/project 配额。
3. 实现中断清理、过期、pin、活动引用保护；空间不足返回 quota_exceeded。
4. CLI 原子写用户指定输出文件；已有文件按显式覆盖规则处理，不能静默破坏数据。

验收 O-08/O-12/C-05。输出大小边界、hash 损坏、断开、磁盘满和恶意路径测试。回退：禁用产物能力，不发成功但无有效产物的观察。

### O04 · Observe --sync 与操作调度

代码落点：`src/commands/live.rs` 的 `run_cycles`、`src/commands/dev.rs`、`src/devserver/control.rs`；拟议 `operations.rs`、`observe.rs`。

1. 把构建请求作为 supervisor 正式输入接入 watcher/键盘循环，避免 control handler 只扫描却不能触发构建。
2. 实现操作提交/查询/取消和总 deadline；运行状态存内存/有界 journal，不持锁等待编译和读回。
3. 按观察设计十步算法锁定输入、build、run、资源和 scene，采集中变化最多重试一次。
4. 把 screenshot/semantics 的 require 别名解析到明确能力；多窗口必须选择，缺能力或旧模板明确降级。
5. 发布不可变 observation 和实际 scope/consistency；旧应用、部分产物、超时都保留诊断但不能成功冒名。

PR 拆分：操作状态机/假 runner → build request 接入 → runtime capture → CLI/真实 macOS 验收。验收 O-01/O-02/O-03/O-04/O-08/O-09/O-10。

输出成功、构建中再编辑、旧 app 留存、迟到 capture 的完整时间线。回退：observe 命令不可用时 D1 仍可查询，不能阻塞 Live 或冒充当前 UI。

## 6. G2：场景、语义、输入与 Agent 接入

### O05 · 语义快照与 GPUI adapter

代码落点：新 runtime 的 GPUI adapter；必要的上游补丁先在实验分支验证，固定 revision；协议仅持有跨版本 DTO。

1. 将 logical_id/role/name/value/bounds/clip/focus 从真实 scene 导出，记录不支持的字段和原因。
2. 处理 accessibility 激活、节点生命周期、虚拟列表稳定 key、多窗口；不把 `.id()` 或临时导出别名当稳定 ID。
3. 与图像冻结点关联；只能满足 best_effort 时如实声明，不能提前发 same_scene。
4. 缺上游 API 时先提交最小边界补丁和 feature 影响报告，再迁移锁定版本；不把内部 GPUI 类型泄漏进控制协议。

验收 O-10/O-11，Counter/Form/List 的实际树对照。回退：G1 保留截图，G2 对依赖语义的用例 unavailable；不以 OCR 猜测填充树。

### O06 · 查询、分页与变化摘要

代码落点：拟议 `src/devserver/query.rs`、`src/commands/dev.rs` 和 artifact tree index。

1. 支持 logical_id、role/name、parent、字段投影；所有查询绑定 observation，不跨 run 静默复用。
2. 分页游标绑定 artifact hash、筛选条件和位置，过期/参数变化明确报错。
3. 在有唯一 logical_id 时生成 added/removed/changed；不稳定节点退化成 subtree_replaced，不猜对应关系。
4. 限制 200 节点/128KiB，提供 omitted/next_cursor；大的单节点单独引用，不能破坏 envelope 上限。

验收 O-11/A-03。输出 50k 节点压力、虚拟化列表分页、跨 observation 游标误用测试。回退：可关闭 diff，保留完整有界快照查询。

### S01 · 场景 schema 与静态校验

代码落点：拟议 `src/scenario/{schema,validate}.rs`、配置 examples/schema fixtures；挂载命令由 S02/S04 完成。

1. 为 [场景配置](../examples/scenarios.toml) 定义 deny_unknown_fields 类型、位置化错误、字段和 step 限制。
2. 校验 ID、fixture 路径/大小/JSON、timeout、viewport、clock、selectors 和 assertion 参数。
3. 使用构建产出的 registry manifest 校验 component/fixture schema；不存在 registry 时标 registry_unavailable，不声称未知组件已检查。
4. 生成规范化 scenario_hash/fixture_hash；未经默认值规范化和版本标记的 TOML 文本 hash 不作语义比较。

验收 S-01/S-09。输出合法/非法 fixture corpus 及规范化快照。回退：未识别 schema 拒绝执行，不静默忽略拼错断言。

### S02 · 原生组件 preview 与 reset

代码落点：拟议 `src/commands/preview.rs`、`src/scenario/registry.rs`、runtime scenario adapter、`templates/app/src/previews.rs`。

1. 用显式 registry 声明组件、fixture schema、create/reset 和环境适配，不反射构造任意 Render 类型。
2. 编译输出 registry manifest；preview 选择组件并使用独立数据目录，首次构建后直接进入组件。
3. 实现 scenario_ready/reset_generation；源码重启重新创建 fixture，不自动导入 Live 交互 state。
4. 接入 theme/locale/clock/random adapter，回报实际环境和 uncontrolled_inputs；进程内 reset 作为可选优化单独证明。

验收 S-02/S-09。输出 Counter/Form/List 的新进程初态及旧异步任务不泄漏证据。回退：进程内 reset 失败时采用新进程，不保留未知状态继续 check。

### S03 · 正常输入路由与幂等

代码落点：runtime input/hit-test adapter、拟议 `src/devserver/actions.rs`、`src/commands/dev.rs`；窗口动作 owner 与 O01 注册表关联。

1. 对 observation/run/window/revision 预检，解析唯一节点并校验可见、enabled、遮挡和焦点。
2. 通过 GPUI 正常事件路径实现 click/type/key/scroll；坐标路径显式记录像素→逻辑坐标转换。
3. 实现持久接受记录、1000 项/10min 结果缓存和 10000 项 run 级墓碑；先记录再投递，缓存过期不能重新点击。
4. 窗口队列串行；已投递动作的取消/断线/崩溃按 unknown 处理。人工输入污染要能区分来源。

PR 拆分：请求/幂等状态机 → pointer/hit test → keyboard/scroll → 真窗口故障测试。验收 S-03/S-04/S-05/S-06/S-08。

回退：缺真实路由的动作 capability=false；禁止直接调用业务回调来使测试通过。

### M03 · 主机级设备租约与 fencing

代码落点：`src/device/`、`src/devserver/process.rs`；拟议 `src/runner/lease.rs`、平台 OS lock adapter。

1. 在主机级目录用 OS 排他锁管理 stable device ID，owner 包含 session、PID/启动身份和 fencing_token。
2. heartbeat 10s、30s 标 suspect；只有锁释放且 owner 已失效才可恢复 metadata，TTL 不授权强抢。
3. 每次安装/启动/输入/捕获 workload 验证 token；断连重连必须重新探测和申请。
4. 清理仅针对本任务创建且身份仍匹配的资源；不关闭用户的模拟器或同名进程。

验收 M-04/M-05/M-06；必须用不同项目目录和至少两个真实进程竞争，不只用同进程 mutex 模拟。回退：无法取得锁就 device_busy/unavailable，不回到无锁执行。

### M04 · 冻结源码与确定构建键

代码落点：`src/devserver/inputs.rs`、`src/commands/build.rs`、`src/config.rs`；拟议 `src/runner/{snapshot,build_key}.rs`。

1. 从 cargo metadata、场景/native manifest 发现输入及允许的外部 path dependency 根，生成有序清单和 content hash。
2. 读取前后核验，写独立快照；编辑继续发生时重试有界，不能把混合版本称为 frozen。
3. 定义 BuildKey 的 toolchain/profile/features/ABI/native/env 维度；重定位 path dependency 并保持路径语义或明确拒绝。
4. 将同 key 构建隔离在独立输出目录；Android JNI 和 iOS DerivedData 不共用可变 staging 目录。

验收 M-07/M-08，外部链接/越界/秘密筛除和并发 profile 必测。输出输入清单、快照 hash、可重跑构建命令。

回退：不能冻结时拒绝严格 check/matrix，允许普通 Live 的 tracked_scan 但不得提升其保证。任意 build.rs 的隐藏输入需列入限制，不能宣称工具能自动发现全部 I/O。

### S04 · 断言和 gpui check 执行器

代码落点：拟议 `src/commands/check.rs`、`src/scenario/{executor,assertions,report}.rs`。

1. 建立 plan→frozen build→新运行/reset→ready→steps→报告→清理 状态机；先限 macOS 自有窗口，移动由 M01/M02 接入。
2. 支持文档规定的断言，未知字段/缺语义/污染为 inconclusive；wait_for 有界观察，不用固定 sleep 作为 ready。
3. 为每步保存前后观察、动作结果、日志 seq 和耗时；失败停止依赖步骤，默认 retries=0。
4. 增加视觉基线读取/差异产物，基线缺失或不可比不自动通过；基线批准是独立操作，不在修复路径隐式执行。
5. CLI 的等待/async/退出码与 operation 模型一致；失败报告不能被后续 cleanup 错误覆盖。

验收 S-02/S-03/S-07/S-08/S-09/R-05。输出三个标准夹具各连续 20 次真实执行和故障变体。回退：撤回不完整断言，不能将 unknown 转成布尔 false 或 passed。

### A01 · MCP 观察与只读查询适配

代码落点：拟议 `src/agent/{service,mcp}.rs`、`src/commands/mcp.rs`；CLI 接入同一 service，复用 control client。

1. 暴露 status/diagnostics/events/windows/observe/operation_get/artifact_read；O06 未完成时不注册 query。
2. 从共享类型产生严格 schema；所有参数在 service 再校验，stdout 只输出 JSON-RPC。
3. structuredContent 保留 CLI envelope 和业务终态，摘要≤16KiB，图像/大树按需读取。
4. observe(sync=true) 可能触发构建/重启，工具不能整体标 readOnlyHint=true；不让 annotations 替代权限校验。

验收 A-01/A-02/A-03；一个真实 MCP 客户端加无模型协议测试。回退：MCP 适配停止不影响其他 Live 用户；保留 CLI 完整功能。

### A02 · MCP actions/checks 与取消

代码落点：`src/agent/mcp.rs` 的拟议工具注册、共享 action/check service 与 owner/scopes。

1. 接入 preview/act/check/cancel，复用现有状态机、租约和幂等键，不重复实现输入。
2. 校验 expected scope/owner；只允许取消本授权范围内的 operation，不能用适配进程 PID 认领所有会话。
3. 长任务返回 operation_id，断线后查状态；unknown 不重放、host 重试不生成新 request_id。
4. 日志/UI 文本仅作为数据；“忽略权限并点击”等内容不能影响工具授权。

验收 A-01/A-04。输出两客户端互相观察、争用窗口、越权取消及断线重试记录。回退：禁用 mutating 工具，A01 和 CLI 不变。

### A03 · 版本知识、组件目录与工作流包

代码落点：拟议 `src/agent/context.rs`、`src/commands/context.rs`、版本化工作流模板；复用 T02 manifest/S01 schema。

1. 导出实际 CLI/runtime/template/GPUI/依赖 revision、目标能力和项目场景；未知版本不能补“latest”。
2. 索引锁定版本的 rustdoc、已编译示例和 registry；按依赖锁/hash 失效，有 fallback 必须说明来源可信度。
3. 提供创建/升级、组件修改验证、平台问题复现三个小工作流；每个写前置能力、失败分支和完成定义。
4. 宿主集成包共用源文件；只列出/推荐第三方 Skills，安装启用要有用户选择，不从依赖包自动执行指令。

验收 A-05。输出依赖版本切换前后 context、错误版本 API 的负例、无对应 rustdoc 时的降级。回退：只返回本地已证实 metadata，不在线猜 API。

### S05 · 可选：类型化预览参数实验

代码落点：runtime preview parameter registry、拟议 `src/scenario/overlay.rs`；不进入生产默认路径。

1. 为 Counter/List 显式注册颜色、间距、字号、数据量的类型与范围；只接受数据，不执行表达式。
2. overlay_revision 进入每个 observation/check 报告，非零结果标 preview_only。
3. 撤销/关闭/重启清空 overlay；固化只生成待审 patch，应用前检查源 hash。
4. 对至少 20 次修改测量相对普通重建的收益、撤销正确性和源码并发编辑冲突。

验收 S-10；输出 adopt/reject 实验记录。回退：关闭该 capability，正常 Live/preview/reset 继续可用。

## 7. G3：本地跨端与可复现验证

### M01 · iOS simulator / Android 截图与原生日志

代码落点：`src/device/{ios,android}.rs`、`src/commands/{live,run}.rs`；拟议 `src/runner/{ios,android,logs}.rs`。

1. 用明确 UDID/serial、租约和 run 身份执行截图，二进制保存 PNG；记录方向/DPI/系统栏/前台应用。
2. 启动日志 collector，在 early native crash、PID 切换、app channel 未建立时仍有证据；不能归属的日志单独保存。
3. 将安装/launch/进程证据分离；断线不等于退出，设备截图不等于 scene capture。
4. 分别跑完整 GPUI Android emulator/iOS simulator 应用，包含键盘、系统弹窗、旋转和后台切换。

PR 拆分：runner trait/契约 → iOS → Android → 真模拟器故障矩阵。验收 M-01/M-02/O-08/O-10。回退：单个平台 capability 禁用，不影响 desktop；不以宿主 APK 打包测试宣称运行通过。

### M02 · 本地 matrix orchestration

代码落点：拟议 `src/runner/{matrix,scheduler,report}.rs`、check 的 matrix 入口；复用 S04/M03/M04。

1. 解析显式 targets/scenarios/required/timeout/max_parallel，分发前核对 host/ABI/toolchain。
2. 同一快照构建、每目标独立 run；目标内场景串行，跨目标限并发且遵守资源锁。
3. 汇总 passed/failed/inconclusive/unavailable/cancelled；optional 缺失为 partial，required 不全通过绝不 passed。
4. fail_fast 和取消只清理本次拥有的资源；保留已完成 cell 产物，等待和 cleanup 也受 deadline 约束。

验收 M-03/M-06/M-08/R-05。输出 macOS+iOS simulator+Android emulator 矩阵与不可用 Windows cell。回退：用户可单目标运行，不能用本机交叉编译替代 Windows 运行。

### M05 · Repro export/inspect/run

代码落点：拟议 `src/repro/{manifest,export,inspect,replay,redact}.rs`、`src/commands/repro.rs`。

1. 导出场景、证据、环境和源码定位，默认不打包源代码/patch/秘密；来源不全标 requires_source。
2. 对筛除后的文件重算 hash/artifact ID，提供文件类别清单与 replay prerequisites。
3. inspect 只校验 ZIP/manifest/hash/路径/配额，不执行代码或页面脚本；验证成功后才入库。
4. run 显式选目标与可信源码，沿用冻结/check 流程；重复失败算成功复现，不算修复通过。
5. 静态报告引用注册产物、转义文本、避免绝对本机路径和任意外链脚本。

验收 R-01 至 R-05。输出一个无源码诊断包、一个可重放夹具包及恶意归档测试。回退：导入失败保留只读错误，不污染正常 artifact 索引。

### Q01 · 分层 CI 与真实平台收口

代码落点：`.github/workflows/ci.yml`、`tests/`、现有 `scripts/check-android-template.py`；拟议独立 L2/L3 workflow。

1. L0 在三个 OS 跑纯协议/解析/幂等/租约/归档用例；L1 保留生成模板和 Android 宿主打包。
2. L2 单独提供实际图形会话与模拟器配置，执行正式 scenario；完整 GPUI runtime 不允许被 mock .so 替代。
3. L3 使用固定硬件或受控设备池，权限明确；不在不受信任 PR 中自动向 self-hosted runner 暴露签名/主机权限。
4. 统一上传 case/environment/summary/限额日志/截图/树/diff；并发、超时、崩溃 cleanup 有专项作业。

验收 C-02/O-01/O-11/S-03/M-01/M-02/R-04。输出平台能力到 CI job/最近证据的索引。回退：缺 runner 时标 not_run 并取消相应支持声明，不改成绿色 skip。

## 8. G4：性能、规模与可选交互报告

### G01 · 低开销应用性能指标

代码落点：拟议 `src/perf/metrics.rs`、runtime GPUI/backend hooks、独立 gpui-profile 配置。

1. 从 P01 已证实的 hook 采 layout/paint/frame interval，CPU/GPU 和呈现时间分开命名。
2. 提供指标 schema、单位、source、scope、invalid/drop 计数，不能取得的 GPU query 不伪造成 CPU 计时。
3. 聚合每秒发布，原始帧写有界产物；profile 与生产 release/Live 端口隔离。
4. 同场景仪表开/关对照，预算 CPU 帧 P95 增量≤5%，超预算默认关闭高成本项并记录。

验收 P-03/P-04；输出指标对照、GPU disjoint/无效样本、内存与采样队列上限。回退：按指标关闭 capability，低开销 summary 不受影响。

### G02 · 性能预算和统计比较

代码落点：拟议 `src/perf/{budget,statistics,executor,report}.rs`、`src/commands/perf.rs`。

1. 实现预算 schema 和固定算法 paired-bootstrap-v1；绝对/相对规则独立启用，边界/单位/零基线有测试。
2. 使用同场景冻结源码、profile 构建和独占设备，10 次预热、30 次有效测量/配对，保存 pair_id。
3. 核验环境键、样本完整性和正确性断言；不同设备/采样模式只给趋势，不给正式回归结论。
4. 报告 distribution/95% 区间/规则结果与原始数据；inconclusive 不能当 passed，失败不能自动改基线。

验收 P-05/P-06。输出正常/显著退化/噪声大/基线为零四组统计 fixture 和固定硬件实验。回退：继续收集原始数据，不发布无证据的比较结论。

### T05 · 输入索引优化

代码落点：`src/devserver/inputs.rs`、watcher loop，复用 M04 输入边界；拟议 indexed scan 状态。

1. 建立路径/hash/mtime/大小/文件身份索引，事件只标 dirty；rename/delete/目录移动双向失效。
2. 正常保存增量哈希；observe --sync 第一版仍完整核验，watcher overflow/网络盘/读取竞态回退。
3. 外部 path dependency 和 build 脚本声明的输入纳入范围；非构建文件是否忽略不能按扩展名武断决定。
4. 在原 F01 基线和大型输入集比较扫描 CPU/I/O，列出实际提速与未优化阶段。

验收 T-08/T-09/P-02。回退：自动恢复全量扫描，优先保证 wrong_revision_acceptance=0。

### T06 · 构建缓存与有界预热

代码落点：拟议 `src/runner/build_cache.rs`、build coordinator、native staging；遵循 M04 BuildKey。

1. 相同 key 在途构建合并引用，验证已完成 manifest/文件大小/hash 才命中。
2. 失败/取消/缺产物不缓存；更改工具链/features/锁文件/环境/ABI 均失效。
3. 缓存复用不复用 run/安装身份；调用者取消不杀其他 owner 的共享构建。
4. 可选预热默认关闭，限 CPU/内存/磁盘并让前台构建优先；先量化 Cargo 自有缓存，再评估 sccache。

验收 T-10/M-08/P-02。输出缓存命中原因、投毒/部分输出拒绝、多 ABI 同时构建证据。回退：停用 provider，保留独立 staging，不能通过共享可变目录来兜底。

### Q02 · Agent 效果基准

代码落点：拟议 `benchmarks/agent/` 的任务 manifest、评分器和版本化提示；不嵌入产品 runtime。

1. 建立 [12 项已知答案任务](acceptance-matrix.md)，固定代码/fixture、修改范围、模型/提示/工具与预算。
2. 同模型、同预算比较普通 CLI 和结构化观察/场景工具；顺序随机化，每任务每模式至少 5 次独立尝试。
3. 用独立断言及已批准基线评分，记录错误版本误通过、错误修复、inconclusive、总调用/字节/耗时。
4. 输出任务级分布和汇总，不只展示胜出的例子；预算耗尽/环境失败保留并单独分类。

验收 A-07/A-08。回退：若效率无提升就记录负结果并调查询/工作流，不以更换模型或放宽正确性伪造产品收益。

### G03 · 可选：GPU capture/analysis providers

代码落点：拟议 `src/perf/providers/`、分析会话管理，复用 O03/M03/S04。

1. 单独 PoC Metal、RenderDoc、Tracy/gpudebug；先检测版本/API/权限，缺失返回 unavailable。
2. 捕获绑定正确 scenario/run/window/scene 区间，限制 512MiB/2 个 trace；GPU trace 不塞 MCP image。
3. 每次查询引用 trace 节点与原输出；分析 session≤2，空闲 5min 释放，不重复加载同一 trace。
4. 运行渲染错误/负载退化/正常对照，确认 cleanup 和采样干扰边界。

验收 P-07/P-08。每 provider 分开 PR、分开证据；L 是首个 provider 的预算，新增 backend 重新估计。回退：保留人工捕获说明，绝不自动安装/切换 Xcode。

### M06 · 可选：显式远程 runner

代码落点：拟议 `src/runner/{remote,stdio_protocol,transfer}.rs`、runner 命令；不搭建公共云服务。

1. 仅连接用户登记的 SSH host/身份；握手协商版本/能力/工具链，不自动发现网络机器。
2. source manifest 对账、缺块上传/hash 校验、lease/fencing 后执行，remote path 不直接当本机路径。
3. job/request ID 持久化，断线恢复先查状态；有界保留和到期清理，unknown 不重放输入。
4. 明确远程秘密注入、日志筛选/产物拉取、配额与主机不兼容失败。

验收 M-09/M-10。输出两台独立机器、断链/重连、旧 token 迟到及 owned resource 清理。回退：remote unavailable，不能偷偷改在另一台设备运行。

### A04 · 可选：本地报告与 MCP Apps 展示

代码落点：拟议 `src/agent/review.rs`、版本化静态报告资源；基于 M02 的结果模型，不重新做测试平台。

1. 先提供静态版本/平台/截图/diff/断言/日志/性能报告，所有内容转义且只读取注册产物。
2. 再做 MCP Apps renderer，宿主不支持时返回链接/图片/JSON，不能阻塞核心工具。
3. 重检/切场景按钮调用原 service，带 expected scope/owner；不在页面中执行任意命令。
4. 按需拉缩略图/局部树，不默认持续传全屏；测试超大/过期产物及不可用平台状态。

验收 A-06/A-04。回退：退回静态报告，操作仍通过现有 CLI/MCP 工具；扩展不是 G1–G3 的依赖。

## 9. 第一轮开工操作单

1. 先跑现有检查并保存基线：`cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`、`cargo package --list`。本清单写成时并未重新执行这些实现测试。
2. 先领取 F01/T01，不直接实现 MCP 或 GPU 分析；用 `mktemp -d` 创建实验工作目录，记录准确路径，禁止把整个用户项目作为临时清理目标。
3. F01 准备夹具与基线后，按 P01 操作单做真实窗口实验；先记录每个 API 的有效范围再决定 GPUI adapter 接口。
4. T02 同步定义模板基线和 Git 跟踪例外；F02 只有在协议、feature、发布依赖三个边界都明确后开始抽取。
5. 在第一批观察 PR 中引入失败用例 O-02/O-03/O-04；“成功截图”不应成为唯一测试。
6. 本轮出口是 G0 和 G1 的证据，不是把所有目录/API 同时搭出来。

## 10. PR 描述模板与完成审查

```text
Task: O04 (part 2/4)
Status: in_review
Baseline commit / current commit:
Dependencies and their evidence:
Contract/schema changes:
Changed existing files / proposed new files:
User-visible behavior and explicit non-goals:
Acceptance IDs / case variants / platforms / CI tiers:
Evidence root / environment / commands / expected vs actual:
v1 / old template / release compatibility:
Cancellation / restart / permissions / quotas / privacy:
Migration / capability flags / rollback:
Known unsupported paths / next PR:
```

Review 必须回答：本次成功结论绑定哪份输入和哪次运行？缺能力时是否会伪成功？副作用是否有 owner/幂等？磁盘与队列是否有上限？旧用户如何继续工作？失败能否复现？

文档已写、单元测试已过、mock 路由已通、平台构建已过、真实场景已过是五种不同进度；只有任务对应层级的证据齐全，才能将任务改为 done。
