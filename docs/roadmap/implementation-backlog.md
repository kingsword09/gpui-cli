# 实施清单：从 D1 到可验证的跨平台开发闭环

状态核查：2026-10-01，主分支 `d7f383a`。35 项中 1 done、21 in_progress、13 planned；
完整代码事实、旧审计问题与验证范围见[当前进度与接续记录](current-status.md)。
原设计基线：`6d091b6`（2026-09-21）；本文不是已完成功能列表。

入口：[总路线](../ROADMAP-agent-native-development.md)。验收编号的完整步骤见 [验收矩阵](acceptance-matrix.md)，接口语义以各 [专项设计](../ROADMAP-agent-native-development.md) 为准。

## 1. 如何执行

每个任务是一个可审查的工作包，不要求硬塞进一个大 PR。表中的依赖是完成该工作包的硬前置；可以提前做不改变能力声明的类型/测试准备。字段中的路径包含最初设计建议；部分已实现，以当前代码为准，不应为凑目录提前创建空实现。

- `planned`：工作包尚未开工，可已有共用基础；`in_progress`：已有实现切片或实验，但工作包仍缺实现/验收；`in_review`：工作包代码与证据齐全，等待审查；`done`：合并且列明的验收通过。
- `blocked_by_experiment`：上游/平台实验没有通过，附实验记录和重试条件；`deferred`：明确延后，不能被统计为已交付。
- S = 0.5–1、M = 1–3、L = 3–5 个有效人日；不含等待设备、上游、签名和 review。超过 L 必须继续拆 PR，估计不是交付日期承诺。
- 优先级 `core` 是该门槛的必需内容；`optional` 必须独立声明 provider/平台支持，不阻塞核心门槛。
- 一个实现 PR 附 `task_id + acceptance_ids + evidence_root`。任务表中引用多个用例时，应说明本 PR 覆盖哪一部分；任务全部完成才可以改为 done。
- CI 从 F01 开始逐项随功能加入，不能等 Q01 最后一次性补测试。Q01 负责贯通并审计已有各层任务。

## 2. 任务与硬依赖总表

| ID | 门槛 | 优先级 | 规模 | 前置任务 | 验收 | 状态 |
| --- | --- | --- | --- | --- | --- | --- |
| F01 | G0 | core | M | — | P-01, P-02 | in_progress |
| T01 | G0 | core | M | — | T-01, T-02, T-03 | in_progress |
| P01 | G0 | core | L | F01, T01 | O-05, O-10, O-11, P-03 | in_progress |
| T02 | G1 | core | M | F01 | T-04 | in_progress |
| F02 | G1 | core | L | P01, T02 | C-01, C-02, C-03, C-04, C-05 | in_progress |
| T03 | G1 | core | M | T02 | T-04, T-05 | in_progress |
| T04 | G1 | core | L | T03, F02 | T-06, T-07, C-02 | in_progress |
| O01 | G1 | core | M | F02, P01 | O-04, O-05 | in_progress |
| O02 | G1 | core | M | F02 | O-06, O-07 | in_progress |
| O03 | G1 | core | M | F02 | O-08, O-12, C-05 | done |
| O04 | G1 | core | L | O01, O02, O03 | O-01, O-02, O-03, O-04, O-08, O-09, O-10 | in_progress |
| O05 | G2 | core | L | P01, F02, O01 | O-10, O-11 | in_progress |
| O06 | G2 | core | M | O05, O03 | O-11, A-03 | in_progress |
| S01 | G2 | core | M | F02 | S-01, S-09 | in_progress |
| S02 | G2 | core | L | S01, O04 | S-02, S-09 | in_progress |
| S03 | G2 | core | L | O06, O04 | S-03, S-04, S-05, S-06, S-08 | in_progress |
| M03 | G2 | core | M | T01, F02 | M-04, M-05, M-06 | in_progress |
| M04 | G2 | core | L | T01, F01 | M-07, M-08 | in_progress |
| S04 | G2 | core | L | S01, S02, S03, M04 | S-02, S-03, S-07, S-08, S-09, R-05 | in_progress |
| A01 | G2 | core | M | O04, O03 | A-01, A-02, A-03 | planned |
| A02 | G2 | core | M | A01, S04, M03 | A-01, A-04 | planned |
| A03 | G2 | core | M | T02, S01, F02 | A-05 | planned |
| S05 | G2 | optional | M | S02, S04 | S-10 | planned |
| M01 | G3 | core | L | O04, T01, M03 | M-01, M-02, O-08, O-10 | in_progress |
| M02 | G3 | core | L | S04, M01, M03, M04 | M-03, M-06, M-08, R-05 | in_progress |
| M05 | G3 | core | L | M02, O03 | R-01, R-02, R-03, R-04, R-05 | planned |
| Q01 | G3 | core | M | F01, O04, S04, M01 | C-02, O-01, O-11, S-03, M-01, M-02, R-04 | planned |
| G01 | G4 | core | L | F01, P01, O01 | P-03, P-04 | planned |
| G02 | G4 | core | L | G01, S04, M04, M03 | P-05, P-06 | planned |
| T05 | G4 | core | M | F01, M04 | T-08, T-09, P-02 | planned |
| T06 | G4 | core | L | T05, M04 | T-10, M-08, P-02 | in_progress |
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

当前模板已交付 `semantics.logical_id` capability 和显式 `declare_logical_id(element_id,
logical_id)` bridge；runtime 只为声明成功且在当前 debug tree 中匹配的节点写入 logical_id，
冲突映射明确失败。GPUI 原生 author/accessibility ID、bounds 与真实 macOS a11y 激活仍需
平台验收或最小上游补丁，不能把 bridge 编译通过当作 O05 完成。

### O06 · 查询、分页与变化摘要

代码落点：拟议 `src/devserver/query.rs`、`src/commands/dev.rs` 和 artifact tree index。

1. 支持 logical_id、role/name、parent、字段投影；所有查询绑定 observation，不跨 run 静默复用。
2. 分页游标绑定 artifact hash、筛选条件和位置，过期/参数变化明确报错。
3. 在有唯一 logical_id 时生成 added/removed/changed；不稳定节点退化成 subtree_replaced，不猜对应关系。
4. 限制 200 节点/128KiB，提供 omitted/next_cursor；大的单节点单独引用，不能破坏 envelope 上限。

验收 O-11/A-03。输出 50k 节点压力、虚拟化列表分页、跨 observation 游标误用测试。回退：可关闭 diff，保留完整有界快照查询。

当前代码已交付 observation 绑定的 query 与 diff control/CLI：查询支持过滤、投影和游标；diff
比较两个成功 observation 的唯一 logical_id，返回 bounded added/removed/changed/unchanged
摘要，并将缺失/重复 ID 的区域降级为 subtree_replaced。真实 GPUI adapter 的稳定 ID 导出、
50k 节点压力、虚拟列表跨帧重定位和大节点独立 artifact 仍未完成，不能把本切片标为 done。

### S01 · 场景 schema 与静态校验

代码落点：`src/scenario.rs`、`src/commands/scenario.rs`、配置 examples/schema fixtures；
S01 提供静态 validate 命令，preview/check 执行挂载由 S02/S04 完成。

1. 为 [场景配置](../examples/scenarios.toml) 定义 deny_unknown_fields 类型、位置化错误、字段和 step 限制。
2. 校验 ID、fixture 路径/大小/JSON、timeout、viewport、clock、selectors 和 assertion 参数。
3. 使用构建产出的 registry manifest 校验 component/fixture schema；不存在 registry 时标 registry_unavailable，不声称未知组件已检查。
4. 生成规范化 scenario_hash/fixture_hash；未经默认值规范化和版本标记的 TOML 文本 hash 不作语义比较。

验收 S-01/S-09。输出合法/非法 fixture corpus 及规范化快照。回退：未识别 schema 拒绝执行，不静默忽略拼错断言。

当前代码已交付 `gpui scenario validate` 静态入口：schema v1 使用 deny-unknown-fields，
校验场景/step ID、路径边界、fixture 内容与哈希、timeout/viewport/clock、selector 和
assertion 参数，并输出规范化 scenario_hash。缺少 `.gpui/registry-manifest.json` 时报告
`registry_unavailable` 警告，不冒充已检查组件；desktop preview/check 已由 S02/S04 接入，
移动场景已通过 M02 matrix/control driver 接入。静态校验不证明运行环境确定性或真实平台验收。

### S02 · 原生组件 preview 与 reset

代码落点：`src/commands/preview.rs`、runtime scenario adapter、`templates/app/src/previews.rs`。

当前已交付 registry manifest schema-v1 的静态读取契约和 desktop preview runtime：生成模板
通过显式 `PreviewRegistry` 在启动时写出 `.gpui/registry-manifest.json`；`gpui preview`
会校验场景、创建独立 preview data dir、以无 snapshot 的新进程启动并等待 runtime 发出
`scenario_ready`。生成 runtime 已按 fixture 选择并渲染 Counter、LoginForm、VirtualList 三种
surface，分别提供稳定语义节点；`gpui dev reset --scenario <id>` 已通过有界控制队列送到
runtime UI 线程，fixture 重新读取并发出新的 `scenario_ready`/`scenario_reset_result`；每次
新进程从 generation 1 开始。reset 现在也按当前 fixture bytes 重新计算 SHA-256，ready 事件
和单场景 CheckReport 使用 runtime fixture identity；真实环境适配和 matrix 报告传播仍需补齐。

1. 用显式 registry 声明组件、fixture schema、create/reset 和环境适配，不反射构造任意 Render 类型。
2. 编译输出 registry manifest；preview 选择组件并使用独立数据目录，首次构建后直接进入组件。
3. 实现 scenario_ready/reset_generation；desktop 三种 fixture surface 已接入，控制 reset 在
   UI 线程递增 generation，不自动导入 Live 交互 state。
4. 接入 theme/locale/clock/random adapter，回报实际环境和 uncontrolled_inputs；进程内 reset 作为可选优化单独证明。

验收 S-02/S-09。当前切片覆盖 desktop 三种 surface 的启动、fixture 初值/节点、manifest、
ready/reset 事件和独立 data dir；LoginForm 的真实文本输入、VirtualList 的 agent 滚动动作、
真实旧异步任务取消、移动端 preview 和 check 执行仍未完成。
回退：进程内 reset 失败时采用新进程，不保留未知状态继续 check。

### S03 · 正常输入路由与幂等

代码落点：runtime input/hit-test adapter、拟议 `src/devserver/actions.rs`、`src/commands/dev.rs`；窗口动作 owner 与 O01 注册表关联。

当前已交付 S03 的 admission 与 pointer-click 子切片：`ActionRequest` 对
observation、window、logical_id 和 click/type_text/key/scroll payload 做严格有界校验；
`gpui dev act` 复用 operation 幂等记录，绑定当前 run/window 并从指定 observation
解析唯一 logical_id，预检 enabled 与 bounds。生成 runtime 只对显式 instrument 的
Counter/LoginForm 左键目标导出实际 prepaint bounds/enabled，并在 UI 线程用
`Window::dispatch_event` 发送正常 GPUI MouseDown/MouseUp；只有目标元素的 bubbled
mouse-up observer 收到事件才结束为 succeeded，业务结果仍标为 unverified。

pointer click 已按窗口 owner connection 串行投递，run/revision/scene/connection 在投递前
再次 fencing；断连、超时或已投递但未确认的结果为 unknown，禁止重放。enabled/bounds
缺失、节点 disabled、selector 缺失/歧义和非左键动作均在投递前明确失败。

keyboard 子切片已交付：`type_text`/`key` 分别通过独立的 app-channel
`text_dispatch`/`key_dispatch` 消息进入生成 LoginForm 的 GPUI focus/key event 路径；
replace/append、Enter、Backspace 和目标事件确认均保留在 runtime/UI 线程。服务端按
`input.keyboard.type_text`/`input.keyboard.key` 能力 admission，并继续执行
observation/run/revision/scene/connection fencing。通用组件的 bounds/focus instrumentation
和真实窗口故障矩阵仍未完成。Scroll 已为任意显式声明的滚动容器建立通用 admission
契约：runtime 每帧登记 `declare_scroll_target`，同时导出 bounds 与
`scrollable: true`；服务端在 `Scroll` 投递前拒绝未声明的节点。生成 VirtualList 已通过
该契约接入正常 GPUI `ScrollWheel` 分发，duration scroll 由 runtime 按有界分段执行，
整数总位移保持守恒。动作故障矩阵已覆盖 queued/delivered、deadline、窗口/scene/owner
fencing、runtime dispatch failure、target miss、取消和 app-channel 断线；已投递动作的
不确定结果统一为 unknown，禁止重放。真实 macOS 窗口注入、遮挡/人工输入污染、平台滚动
惯性和触控板 provider 仍未完成。真实 macOS window screenshot 的 limited-adopt 证据见
[S04 macOS window evidence](../experiments/S04-macos-window-evidence-2026-09-27.md)，但
真实生成模板 scene completion 与截图 revision 绑定的验收见
[O04 scene completion live](../experiments/O04-scene-completion-live-2026-09-27.md)。动作
query 已接入可选 `clip_bounds` 与显式
`obscured` 语义：中心命中点不可见时返回 `element_not_visible`，provider 明确报告遮挡时
返回 `element_obscured`，缺字段不会被猜测为可见。

1. 对 observation/run/window/revision 预检，解析唯一节点并校验可见、enabled、遮挡和焦点。
2. 通过 GPUI 正常事件路径实现 click/type/key/scroll；坐标路径显式记录像素→逻辑坐标转换。
3. 实现持久接受记录、1000 项/10min 结果缓存和 10000 项 run 级墓碑；先记录再投递，缓存过期不能重新点击。
4. 窗口队列串行；已投递动作的取消/断线/崩溃按 unknown 处理。人工输入污染要能区分来源。

PR 拆分：请求/幂等状态机、pointer/hit test 与生成 VirtualList scroll → keyboard → duration
scroll（当前已交付）→ 通用滚动容器显式声明契约（当前已交付）→ 动作故障矩阵（当前已
交付 app-channel/window-owner 边界）→ clip/obscured admission（当前已交付）→ 真窗口
遮挡/人工输入故障测试。
验收 S-03/S-04/S-05/S-06/S-08。

回退：缺真实路由的动作 capability=false；禁止直接调用业务回调来使测试通过。

### M03 · 主机级设备租约与 fencing

代码落点：`src/device/`、`src/devserver/process.rs`；拟议 `src/runner/lease.rs`、平台 OS lock adapter。

1. 在主机级目录用 OS 排他锁管理 stable device ID，owner 包含 session、PID/启动身份和 fencing_token。
2. heartbeat 10s、30s 标 suspect；只有锁释放且 owner 已失效才可恢复 metadata，TTL 不授权强抢。
3. 每次安装/启动/输入/捕获 workload 验证 token；断连重连必须重新探测和申请。
4. 清理仅针对本任务创建且身份仍匹配的资源；不关闭用户的模拟器或同名进程。

验收 M-04/M-05/M-06；必须用不同项目目录和至少两个真实进程竞争，不只用同进程 mutex 模拟。回退：无法取得锁就 device_busy/unavailable，不回到无锁执行。

当前已交付 src/runner/lease.rs 的 host-level OS file lock、durable owner/fencing token、
后台 heartbeat 和 owner mismatch 防护；路径 symlink、跨项目同设备争用、Android TCP serial
文件名和 fencing lost 已有纯 Rust 验证。`commands/run.rs` 与 `commands/live.rs` 已将
iOS/Android 的安装、启动、Android 配置/资源写入包进 lease fencing；iOS/Android 原生 PNG
截图命令及 `gpui device capture` 也已通过同一 lease 接入。`gpui check --matrix` 的移动
scenario driver 现在复用该 lease 做 native capture；#211 又让 matrix supervisor 持有唯一
lease、preview 子进程使用 owner delegation，并把实际 run 的 capture/native-log/stop evidence
写入 `CheckContext.mobile_evidence`；#213 又让 Android capture best-effort 补充逻辑 viewport、
scale、方向和保守前台包名，并在 probe 缺失时保留 unknown；设备重连状态机、完整截图 artifact
manifest 和真实设备矩阵证据仍未完成，不能把当前测试写成 M01/M02 的真实设备验收。记录见
[M03 device lease](../experiments/M03-device-lease-2026-09-28.md)。

### M04 · 冻结源码与确定构建键

代码落点：`src/devserver/inputs.rs`、`src/commands/build.rs`、`src/config.rs`；拟议 `src/runner/{snapshot,build_key}.rs`。

1. 从 cargo metadata、场景/native manifest 发现输入及允许的外部 path dependency 根，生成有序清单和 content hash。
2. 读取前后核验，写独立快照；编辑继续发生时重试有界，不能把混合版本称为 frozen。
3. 定义 BuildKey 的 toolchain/profile/features/ABI/native/env 维度；重定位 path dependency 并保持路径语义或明确拒绝。
4. 将同 key 构建隔离在独立输出目录；Android JNI 和 iOS DerivedData 不共用可变 staging 目录。

验收 M-07/M-08，外部链接/越界/秘密筛除和并发 profile 必测。输出输入清单、快照 hash、可重跑构建命令。

回退：不能冻结时拒绝严格 check/matrix，允许普通 Live 的 tracked_scan 但不得提升其保证。任意 build.rs 的隐藏输入需列入限制，不能宣称工具能自动发现全部 I/O。

当前已交付 M04 的稳定扫描/副本切片和 Cargo scope 切片：`Inputs::scan_stable` 对
sources/assets/目录 symlink manifest 执行最多两次重扫，连续变化明确失败；
`Inputs::freeze_to` 可将稳定 manifest 复制到 root 外并重核验；
`CargoInputScope::discover` 使用锁定的完整 cargo metadata 发现并筛选外部 path package
根，拒绝 workspace 祖先越界和 symlinked package root；
`Inputs::freeze_to_with_cargo_scope` 已将这些 root 复制到快照并重写基本 Cargo path。
记录见
[M04 input stability](../experiments/M04-input-stability-2026-09-27.md)。外部 root 尚未
覆盖 native/scenario manifest 和 build.rs 隐藏输入；
FrozenInputs 现对 local.properties/keystore.properties 只保留相对路径声明，不读取内容
或复制进快照；该精确文件名过滤不是通用 secret scanner。随后 #205 为受控 Android
custom/release signing 增加独立的项目内 keystore 扩展名过滤和短生命周期 `0600` 副本，记录见
[M04 frozen sensitive input filter](../experiments/M04-frozen-sensitive-input-filter-2026-09-28.md)。
`src/runner/build_key.rs` 已补齐 BuildKey 维度/规范化/摘要，并已接入普通构建路径与 T06
产物缓存。当前 desktop `build`/`run` 已通过
`src/runner/build_inputs.rs` 组合稳定源码 manifest、Cargo.lock、NativeInputs、
`rustc -vV` 和显式环境 allowlist，使用真实 BuildKey 生成
`.gpui/builds/desktop/<key>/cargo-target` 并设置 `CARGO_TARGET_DIR`；见
[M04 desktop BuildKey](../experiments/M04-desktop-build-key-2026-09-27.md)。
当前 desktop build plan 还会先创建临时 `FrozenInputs` 副本，将 Cargo workspace 和允许的
外部 path package 作为冻结工作根执行；见
[M04 desktop frozen build root](../experiments/M04-desktop-frozen-build-root-2026-09-27.md)。
非 live 的 iOS `build`/`run` 也已使用 iOS target 对应的 BuildKey，将 Cargo target 和
Xcode DerivedData 放入 `.gpui/builds/ios/<key>`；见
[M04 iOS BuildKey](../experiments/M04-ios-build-key-2026-09-27.md)。Android 多 ABI 的
非 live `build`/`run` 也已使用 ABI 集合对应的 BuildKey，将 Cargo target、JNI staging
和 Gradle app build 放入 `.gpui/builds/android/<key>`；见
[M04 Android BuildKey](../experiments/M04-android-build-key-2026-09-27.md)。Android key 另纳入
SDK platforms/build-tools package revision、NDK revision、cargo-ndk 与 Java 版本摘要；身份
不可读或 SDK/NDK 环境冲突时关闭 Android cache hit，记录见
[M04 Android toolchain fingerprint](../experiments/M04-android-toolchain-fingerprint-2026-09-28.md)。
iOS key 另纳入当前 Xcode build 与目标 SDK version/build 指纹；身份不可读时关闭 simulator
cache hit，记录见
[M04 iOS Xcode/SDK fingerprint](../experiments/M04-ios-xcode-sdk-fingerprint-2026-09-28.md)。
live preview builder 已接入 source-project target-specific output root，并在该 root 上取得跨进程
`BuildOutputLock`；#169 又让 desktop preview 通过独立 verified artifact manifest 在验证成功时
跳过 Cargo；#171 又让 iOS simulator live preview 在同一语义下验证完整 `.app` bundle 并跳过
rustup/Cargo/XcodeGen/`xcodebuild`；#173 又让 Android default-debug live preview 在同一语义下
验证 JNI/APK 输出并跳过 rustup/cargo-ndk/Gradle；这仍不等于 iOS physical、Android release-only/
复杂/远端 signing live-preview 命中、跨命令 ownership 或 coalescing。#205 已覆盖受控 Android
local custom/release signing 的非 live build/run 命中，#209 又覆盖显式 debug custom-signing live
preview 的 manifest/coordinator 复用，记录见
[T06 Android signing BuildKey](../experiments/T06-android-signing-build-key-2026-09-30.md)。
这只串行进入该锁路径的输出变更，不等于构建已完成或可复用。snapshot build
orchestration、同 key 在途任务合并和 `build.rs` 隐藏输入
尚未接入。非 live 命令也已接入各自的临时 `FrozenInputs` 工作根；desktop 的切片见
[M04 desktop frozen build root](../experiments/M04-desktop-frozen-build-root-2026-09-27.md)，
iOS 的切片见
[M04 iOS frozen build root](../experiments/M04-ios-frozen-build-root-2026-09-27.md)。Android
非 live 命令的切片见
[M04 Android frozen build root](../experiments/M04-android-frozen-build-root-2026-09-27.md)。
`src/runner/build_manifest.rs` 已提供绑定 platform/BuildKey、逐文件 size/hash 校验和
原子发布的 artifact manifest 基础；记录见
[M04 build artifact manifest](../experiments/M04-build-artifact-manifest-2026-09-27.md)。
非 live Android 构建成功后已发布覆盖 JNI staging 与 APK variant 输出的 manifest；记录见
[M04 Android artifact manifest](../experiments/M04-android-artifact-manifest-2026-09-28.md)。
非 live iOS 构建成功后也已发布完整 `.app` bundle manifest；记录见
[M04 iOS artifact manifest](../experiments/M04-ios-artifact-manifest-2026-09-28.md)。
非 live desktop 构建也已登记 Cargo JSON 返回的实际 package binary；记录见
[M04 desktop artifact manifest](../experiments/M04-desktop-artifact-manifest-2026-09-28.md)。
普通 `build`/`run` 的 snapshot BuildKey orchestration 与同 key 在途任务合并已由 #177 接入：可复用
desktop/iOS/Android 路径以 `.build-coordinator.json` 选举 leader，follower 以 OS-locked subscriber
等待并复核 artifact manifest；leader 消失可接管，failed attempt 在无活跃 subscriber 后可重试。
#179 又让 desktop live preview/check 使用独立的 `.preview-build-coordinator.json` 与 preview manifest
verifier；它与普通 build/run attempt 隔离，同时共享 output lock 串行访问 Cargo target。#181 又让
iOS simulator preview 复用该 coordinator，#183 再让 Android default-debug preview 验证 JNI/APK
manifest 和 debug keystore 指纹；physical device、release/custom-signing、cache-disabled 和
BuildKey 不可复用路径仍保留原有锁流程。含未建模输入的路径继续不共享。
#185 允许 superseded 的 follower 释放自己的 subscription；#187 让 leader 也持有 subscriber，#189
按 attempt 安全统计 active references。#191 又让 leader revision 失效时终止 owned process tree，写入
retryable superseded terminal marker，使旧 attempt 的 followers 释放引用并重新竞争；follower 的取消
不会终止仍 current 的 leader。subscriber 注册、计数、coordinator record 与 cache-clean 检查共用短时
state lock，避免刚创建的 subscriber 被误判为 stale。该机制尚不是完整 last-reference 状态机：没有
`cancelled`/`partial` 终态，active count 不授权忽略 leader 自己的 revision；#201 又增加 coordinator schema v2
的 owner fencing token 和 heartbeat，stale heartbeat 只作诊断，terminal publish 必须重新验证当前 owner，
接管仍需要 output OS lock。
#193 又增加 caller-cancel reason：leader 在 state lock 内先释放自身引用再计数；有 follower 时发布可复用
terminal result 并只让取消的 leader 返回 cancellation，无 follower 时发布 retryable cancellation marker。
#195 又把 leader control 接到 desktop/iOS simulator/Android default-debug preview 的 owned process loop：
最后引用 caller-cancel 终止整个 process tree，有 follower 时释放 leader 引用并继续构建；superseded
仍终止旧 attempt 并让 follower 重新竞争。`Build::is_current_for_coordinated_work` 在共享期间不误杀
仍被 follower 需要的构建。#197 增加显式 `Cancelled` terminal state：无 follower 可消费的 caller-cancel
不再伪装成普通失败；waiter 释放旧 subscription 并重新竞争，普通编译错误仍为 `Failed`，superseded
仍是 retryable `Failed` marker。
#199 增加 `Partial` terminal state：调用方显式返回 partial marker 时保留该诊断；attempt 不验证、共享或
命中不完整输出，follower 放弃旧引用并重新竞争。它不提供 partial artifact manifest、恢复或渐进消费。
#203 又将 physical iOS 的 code-signing identity/profile 摘要纳入 BuildKey；签名输入缺失或变化时保持
cache bypass/拒绝发布，签名可用时允许 physical manifest 命中。#205 又为受控 Android local
custom/release signing 纳入 properties/keystore 摘要并接入非 live build/run；#209 又让显式
`buildTypes.debug.signingConfig` 的 custom-debug live preview 复用 signing fingerprint、verified
manifest 和 preview coordinator；release-only、复杂/远端 signing 与隐藏输入仍未建模。
M04 仍未完成：preview/check orchestration 的其余部分，以及
`build.rs`/Gradle/NDK/Xcode 隐藏输入尚未接入。#155 已让单场景 desktop `check` 调用
`desktop_build_plan`，#157 又让 matrix 在 admission 前创建一个共享 workspace snapshot，
从快照读取 scenario/matrix 并让所有 cell preview 从同一 runtime root 启动；原项目 root 仍仅
用于 baseline/diff、移动 artifact 和 lease 路径。单场景 context 保留 snapshot hash/BuildKey，
matrix per-cell context 现在也保留 shared snapshot hash、runtime environment 和 #161 的
target-specific BuildKey；#163 又让 preview builder 使用 source-project 的 target/JNI/Gradle/
DerivedData 输出布局，并在同一 supervisor 内串行同 key cell；#165 再让 desktop/iOS/Android
preview builder 在对应 output root 取得跨进程 `BuildOutputLock`，但不把 key/layout/lock evidence
伪造成构建完成或复用。`BuildOutputLock` 在取得 OS 锁后还会原子发布 `.build-owner.json`，记录当前
owner、PID、开始时间、状态和可选 key hash；guard 释放时仅删除仍匹配自身 owner_id 的记录，stale
record 会在下一个持锁者取得 OS 锁后被覆盖，cache clean 会排除该记录的大小。该文件只是跨进程
ownership 证据，OS 锁仍是活跃性唯一权威，不提供 heartbeat/fencing 或订阅者取消协调；普通
build/run 的 coordinator 记录、subscriber 锁和 manifest 复核见
[S04 BuildKey coordinator](../experiments/S04-build-coordinator-2026-09-29.md) 与
[S04 desktop preview coordinator](../experiments/S04-preview-build-coordinator-2026-09-30.md) 与
[S04 iOS preview coordinator](../experiments/S04-ios-preview-build-coordinator-2026-09-30.md) 与
[S04 Android preview coordinator](../experiments/S04-android-preview-build-coordinator-2026-09-30.md) 与
[S04 preview coordinator cancellation](../experiments/S04-preview-coordinator-cancellation-2026-09-30.md) 与
[S04 leader subscriber reference](../experiments/S04-leader-subscriber-reference-2026-09-30.md)。
[S04 subscriber identity/count](../experiments/S04-subscriber-identity-count-2026-09-30.md) 与
[S04 superseded leader cancellation](../experiments/S04-superseded-leader-cancellation-2026-09-30.md) 与
[S04 last-reference cancellation](../experiments/S04-last-reference-cancellation-2026-09-30.md)。
[S04 preview last-reference wiring](../experiments/S04-preview-last-reference-wiring-2026-09-30.md)。
local `build.rs`
等未建模输入会让严格路径直接不可用，不回退到可变目录。`CARGO_ENCODED_RUSTFLAGS` 的已确认
allowlist 遗漏已由 #149 修复，但其他输入遗漏、跨命令共享构建和冻结执行边界仍有效，
见[当前审计](current-status.md)。

T06 的 desktop 首个缓存切片以 BuildKey 级 OS 文件锁串行请求，manifest 完整校验后跳过
Rust 编译；记录见
[T06 desktop manifest cache hit](../experiments/T06-desktop-cache-hit-2026-09-28.md)。
后续 iOS simulator 切片在相同锁与完整 app-bundle manifest 校验下跳过 Cargo/Xcode 构建；
真机因签名输入尚未纳入 BuildKey 而继续重建，记录见
[T06 iOS simulator manifest cache hit](../experiments/T06-ios-simulator-cache-hit-2026-09-28.md)。
Android default-debug 切片在 BuildKey 纳入 debug keystore 指纹并完整验证 JNI/APK 输出后
允许命中；#205 又为受控 local custom/release signing 的非 live build/run 纳入签名输入摘要并
允许命中；#209 又让显式 debug custom-signing live preview 纳入 signing fingerprint，并在
verified manifest/coordinator 校验通过时命中；release-only、复杂/远端 signing 仍 bypass；记录见
[T06 Android debug manifest cache hit](../experiments/T06-android-debug-cache-hit-2026-09-28.md) 和
[T06 Android signing BuildKey](../experiments/T06-android-signing-build-key-2026-09-30.md)。
Android-template CI 另以真实 cargo-ndk 与 Gradle 构建最小 cdylib 两次，验证 Android CLI
第一次 miss、第二次同 key hit 和 APK ABI；它不包含完整 GPUI app 或设备运行，记录见
[T06 Android CLI cache smoke](../experiments/T06-android-cli-cache-smoke-2026-09-28.md)。真实在途
preview/check 任务共享仍未接入；#177 已将普通 build/run 接入共享 coordinator，但取消引用/终止、
queued 状态和预热仍未实现；coordinator `Partial` 仅作为不可复用的诊断终态；本地 `build.rs` 项目已保守 bypass artifact cache reuse，记录见
[T06 build-script cache bypass](../experiments/T06-build-script-cache-bypass-2026-09-28.md)。
`gpui cache clean --max-bytes` 已提供按 BuildKey 大小预算的显式清理，
活动锁和不安全目录会跳过，记录见
[T06 cache cleanup](../experiments/T06-cache-cleanup-2026-09-28.md)。

PR #169 又让 desktop preview 在受控 BuildKey output root 下写入独立 preview artifact
manifest；下一次 preview 只有在 platform、key hash、文件集合、内容 hash 和唯一可执行文件
均验证通过时才跳过 Cargo，否则回退正常构建。该切片不覆盖 iOS/Android preview 命中、跨命令
构建所有权或在途任务 coalescing，记录见
[T06 desktop preview cache hit](../experiments/T06-desktop-preview-cache-hit-2026-09-29.md)。

PR #171 将相同的 verified-manifest 语义接入 iOS simulator live preview：成功构建后发布完整
`.app` bundle manifest，下一次 live build 只在 iOS platform、BuildKey、文件集合、内容 hash
和预期 simulator app 根全部匹配时跳过 rustup/Cargo/XcodeGen/`xcodebuild`。physical-device
路径仍每次重建，因为本机签名 identity、Provisioning Profile 等输入尚未纳入 BuildKey；该切片
也不覆盖 Android preview、跨命令构建所有权或在途任务 coalescing，记录见
[T06 iOS preview cache hit](../experiments/T06-ios-preview-cache-hit-2026-09-29.md)。

PR #173 将 verified-manifest 语义接入 Android default-debug live preview：matrix 将 ABI、
cache policy 和 default `~/.android/debug.keystore` 内容 hash 传入 preview；只有 policy 允许、
keystore 未变化、Android platform/BuildKey/ABI 匹配，且 JNI staging 与 Gradle debug APK 输出
目录的全部文件通过 hash/size 校验时才跳过 rustup/cargo-ndk/Gradle。release、custom 或敏感
signing、工具链身份不可读和 keystore 变化均不命中；记录见
[T06 Android preview cache hit](../experiments/T06-android-preview-cache-hit-2026-09-29.md)。

### S04 · 断言和 gpui check 执行器

代码落点：拟议 `src/commands/check.rs`、`src/scenario/{executor,assertions,report}.rs`。

1. 建立 plan→frozen build→新运行/reset→ready→steps→报告→清理 状态机；先限 macOS 自有窗口，移动由 M01/M02 接入。
2. 支持文档规定的断言，未知字段/缺语义/污染为 inconclusive；wait_for 有界观察，不用固定 sleep 作为 ready。
3. 为每步保存前后观察、动作结果、日志 seq 和耗时；失败停止依赖步骤，默认 retries=0。
4. 增加视觉基线读取/差异产物，基线缺失或不可比不自动通过；基线批准是独立操作，不在修复路径隐式执行。
5. CLI 的等待/async/退出码与 operation 模型一致；失败报告不能被后续 cleanup 错误覆盖。

当前已交付 S04 的 runtime-agnostic 核心：`src/scenario/executor.rs` 提供 bounded
`CheckPlan`、注入式 `ScenarioRunner`、逐步 `CheckReport`，并覆盖 normal input、wait_for、
assert、capture 的状态转移。断言支持 schema v1 的语义字段、runtime error 和 screenshot
比较；缺少语义/字段/不可比视觉证据返回 `inconclusive`，已投递但未确认的动作不会被重放或
伪造成失败。每步保留 observation/log 序号、action/断言/capture 证据；终止步骤后的依赖
步骤显式 `skipped`，cleanup 错误不会覆盖原始失败或 unknown；初始 prepare/reset/observation
失败时先调用 finalization，再执行 cleanup，以保留已启动移动 preview 的 native-log/post-run
evidence；action operation 错误详情包含 operation ID 时，step action evidence 保留其状态和
ID，unknown 仍为 inconclusive 且不得重放或伪造成 passed；移动 preview 在 `scenario_ready`
之前 launch/registration 等待失败时，也会保留可取得的 native-log/stop/lease evidence，标记
`run_id_bound=false`，不生成没有 steps 的伪造 `CheckReport`；cleanup 的 stop/release 错误也
保留在 `mobile_evidence.cleanup_errors`，stop 失败不能跳过 lease release。matrix executor
在串行和并行 cleanup 后重新读取 runner context，移动 adapter 因而能把 launch、capture、
native logs、stop、lease release 和 cleanup errors 等 cleanup-finalized evidence 写入
`MatrixReport.context.mobile_evidence`；capture-only cell 保留这些证据但不生成伪造的 scenario
`CheckReport`。两条移动路径也将 `RunnerInfo`/`RunnerCapabilities` 写入该 evidence，绑定
runner、host、平台、架构、设备类型和声明能力；这些 metadata 不等于实际设备探针。已有
`EvidenceLog` 也会作为有界事件序列一并保留。记录见
[S04 check core](../experiments/S04-check-core-2026-09-28.md)。

当前 desktop 接线已交付：`gpui check --scenario <id> --target desktop` 启动隔离的
`gpui preview` 子进程，通过同一 control/app channel 等待 `scenario_ready`，再执行
reset→observe→action/wait/assert/capture，并在报告后终止子进程。动作继续只接受
`logical_id` selector；role/name selector 在路由前明确 `unavailable`。语义 query 将 bounded
节点和 bounds/clip 信息接入断言，runtime issue 按 reset 前 event seq 过滤。截图 artifact
已实际采集并由 `screenshot_matches` 接入严格 baseline loader：check 有界读取 published
PNG artifact、校验 chunk/hash、构造实际 key 并把 artifact/key/result 写入断言报告；视觉
mismatch 在可解码时另生成 `.gpui/checks/` diff；缺少
backend/font fingerprint、baseline 缺失或不可比仍保持 `inconclusive`，不自动批准或更新。
记录见
[S04 desktop check](../experiments/S04-check-desktop-2026-09-28.md)。

2026-09-29 增量：单场景和 matrix 的 preview 已使用唯一 session key 定向发现 control，
修复误连已有 preview 的路径；移动场景经 `gpui check --matrix` 接入，单场景入口仍限
desktop。`CheckReport.context` 已保留 runtime ready/reset generation、environment 和
uncontrolled inputs。#151 已让 desktop check 通过 process group/Windows Job Object
终止 owned process tree，并覆盖后代持有输出管道的回归测试；#153 已让 reset/runtime/report
保持 fixture identity 一致。#155 让单场景 desktop check 使用重核验的冻结 workspace snapshot，
并在 context 中写入 snapshot hash/BuildKey；#157 让 matrix 在 admission 前创建一个共享
snapshot，所有 cell 复用其 runtime root/hash。完整 GUI check cleanup/reset probe、真实环境
适配、跨命令共享构建仍需补齐，不能据这些局部修复标记 S04
完成。记录见
[S04 frozen single check](../experiments/S04-frozen-single-check-2026-09-29.md) 和
[S04 frozen matrix snapshot](../experiments/S04-frozen-matrix-snapshot-2026-09-29.md) 以及
[S04 matrix context](../experiments/S04-matrix-context-2026-09-29.md) 和
[S04 matrix target BuildKey](../experiments/S04-matrix-target-build-key-2026-09-29.md) 以及
[S04 matrix build output layout](../experiments/S04-matrix-build-output-layout-2026-09-29.md) 以及
[S04 matrix cell reports](../experiments/S04-matrix-cell-reports-2026-09-29.md)。
当前 context 已随单场景 `CheckReport` 和每个 control scenario `MatrixCellResult` 输出；
matrix control cell 现在也保留完整 steps/证据/cleanup，#211 又让移动 preview 使用 supervisor-owned
delegated lease，并把实际 run 的 capture/native-log/stop evidence 写入
`CheckContext.mobile_evidence`；#213 又将 Android capture 的逻辑 viewport、scale、方向和保守
前台包名写入 artifact/check evidence，缺失 probe 保持 unknown；admission-unavailable 与
capture-only mobile lifecycle cell 不生成伪造 scenario report；#223 又让 executor 在 cleanup 后
重新读取 runner context，因此 capture-only cell 的 launch/capture/native-log/stop/release/cleanup
evidence 也会进入 `MatrixReport`；#225 又把 runner identity/capability metadata 写入两条移动
evidence 路径；#227 又把 install/launch/capture/process/channel/log/stop event log 写入报告。
移动完整语义/输入和真实设备验收仍未完成。

视觉 baseline 静态契约已单独交付：`src/scenario/baseline.rs` 读取项目内
`dev/baselines/<target>/<baseline_id>/manifest.json`，拒绝越界/符号链接/超限文件，校验
BaselineKey、PNG 尺寸和 `sha256:`；缺失、损坏、key/DPI/locale/scope 不一致分别报告
`baseline_missing`、invalid 或 not comparable。当前算法固定为 `exact-sha256-v1`，只返回
matched/different/not_comparable；check 不创建或更新基线，显式 review 入口及 history
记录见
[S04 visual baseline contract](../experiments/S04-visual-baseline-2026-09-28.md)；desktop
artifact 接线和报告证据见
[S04 baseline check wiring](../experiments/S04-baseline-check-wiring-2026-09-28.md)。

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

代码落点：`src/device/{ios,android}.rs`、`src/commands/{live,run}.rs`、
`src/runner/mobile.rs`；拟议 `src/runner/{ios,android,logs}.rs`。

1. 用明确 UDID/serial、租约和 run 身份执行截图，二进制保存 PNG；记录方向/DPI/系统栏/前台应用。
2. 启动日志 collector，在 early native crash、PID 切换、app channel 未建立时仍有证据；不能归属的日志单独保存。
3. 将安装/launch/进程证据分离；断线不等于退出，设备截图不等于 scene capture。
4. 分别跑完整 GPUI Android emulator/iOS simulator 应用，包含键盘、系统弹窗、旋转和后台切换。
5. Android capture 通过 display probe 记录像素/逻辑尺寸、density-derived scale、方向和明确
   foreground marker；命令或厂商输出缺失时报告 unknown，不从 PNG 猜测环境。

PR 拆分：runner trait/契约（已交付）→ iOS simulator adapter（已交付基础路径）→ Android adapter
（已交付基础路径）→ Android process identity（已交付）→ iOS simulator process probe（当前切片）
→ mobile fault evidence boundary（当前切片）→ delegated lease/same-run evidence（已交付切片）
→ Android display evidence（已交付）→ 真模拟器故障矩阵。验收
M-01/M-02/O-08/O-10。回退：单个平台 capability 禁用，不影响 desktop；不以宿主 APK 打包
测试宣称运行通过。契约记录见
[M01 runner contract](../experiments/M01-runner-contract-2026-09-28.md)。

### M02 · 本地 matrix orchestration

代码落点：`src/runner/matrix.rs` 已交付 plan/scheduler/report 契约，
`src/runner/matrix_admission.rs` 已交付配置展开与 admission，`matrix_executor.rs` 已交付
并行执行与资源锁；`src/commands/check.rs` 已接入 matrix CLI 和 desktop/mobile control
scenario driver。#157 已接入 M04 的共享冻结 snapshot 输入边界，#159 已将 per-cell context
传入 MatrixReport，#161 已将 target-specific BuildKey 写入 context，#163 已绑定 source-project
output layout 并串行同 key cell，#165 已让 preview builder 锁定对应 output root，#167 已让
control scenario cell 保留完整 CheckReport；#211 又接入 supervisor-owned delegated lease、实际
run 的 capture/native-log/stop evidence 和 `CheckContext.mobile_evidence`；#213 又将 Android
display evidence 传入 capture/check evidence；#215 又让初始 prepare/reset/observation 失败的
scenario 先 finalize 再 cleanup，保留已启动移动 preview 的 post-run evidence；#217 又在 action
operation 错误详情含 ID 时保留 step action status/operation ID，unknown 仍不重放且保持
inconclusive；#219 又覆盖 pre-`scenario_ready` launch/registration failure，返回
`mobile_evidence`/artifact ids 并明确 `run_id_bound=false`，不生成伪造 scenario report；#221
又把 cleanup stop/release 错误写入 mobile evidence 并保证 lease release 不被 stop failure
跳过；#223 又让 matrix executor 在 cleanup 后读取 runner context，把 cleanup 才完成的
stop/release/error evidence 传播到 `MatrixReport`，并为 capture-only cell 保留 lifecycle evidence
而不生成 scenario report；#225 又写入 RunnerInfo/RunnerCapabilities 作为声明性环境绑定；跨命令
共享构建所有权、移动完整语义/输入 scenario 和真实矩阵仍未接入；已有 EvidenceLog 的报告传播
已接通，但真实 fault matrix 仍未接入。

1. 解析显式 targets/scenarios/required/timeout/max_parallel，分发前核对 host/ABI/toolchain。
2. 同一快照构建、每目标独立 run；目标内场景串行，跨目标限并发且遵守资源锁。
3. 汇总 passed/failed/inconclusive/unavailable/cancelled；optional 缺失为 partial，required 不全通过绝不 passed。
4. fail_fast 和取消只清理本次拥有的资源；保留已完成 cell 产物，等待和 cleanup 也受 deadline 约束。

PR 拆分：matrix plan/status/summary 契约与纯 scheduler（已交付）→ runner execution adapter
（已交付）→ target/scenario admission 与 host/ABI/toolchain preflight（已交付）→
跨目标并行与 supervisor 内资源锁（已交付）→ mobile runner cell lifecycle（已交付）→
matrix CLI 与 desktop scenario factory（已交付）→移动 scenario driver 的
control/native capture、session fencing、delegated lease 和 report context（已交付切片）→ frozen build、host inventory 完整重连和真实矩阵
验收。

当前契约、移动 scenario driver 边界与 admission 记录见
[M02 matrix contract](../experiments/M02-matrix-contract-2026-09-28.md)。

验收 M-03/M-06/M-08/R-05。当前已有本机 Android emulator 的 capture-only 真实探针和
artifact 引用，但仍需输出完整 macOS+iOS simulator+Android emulator 矩阵与不可用 Windows
cell；semantics-required 移动场景必须继续按 runtime capability 返回 unavailable。回退：用户可
单目标运行，不能用本机交叉编译替代 Windows 运行。

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

代码落点：`src/runner/build_cache.rs`、`src/commands/cache.rs` 已实现产物缓存/清理，preview
build 另已接入 output-root lock、`.build-owner.json` ownership record 和支持目标的 preview/check
coordinator；有界预热仍拟议。遵循 M04 BuildKey；现有切片与输入遗漏见 M04 和
[当前审计](current-status.md)。

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
