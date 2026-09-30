# 主分支进度与接续记录

更新日期：2026-09-30（Asia/Shanghai）。核查代码：`0e05087`（PR #199 squash merge）。
本轮 fetch 后，本地 `main` 与 `origin/main` 均指向该提交。后续提交须重新核对，本文不是动态状态。

本文接续 2026-09-28 对 `a6aa685` 的审计，替代其“当前进度”结论；旧报告保留为历史证据。
任务状态以[实施清单](implementation-backlog.md)为准，完成标准以[验收矩阵](acceptance-matrix.md)为准。
专项设计中的目标接口和历史实验记录不能直接当作当前实现或全平台验收。

## 1. 当前结论

基础 CLI/Live 已有实现，macOS 窗口观察有限可用；场景、输入、check、视觉基线和本地 matrix
已接入代码。单场景 desktop check 和 matrix check 均已接入严格冻结输入路径；matrix admission
前创建的 workspace snapshot 由所有 cell 复用，per-cell runtime context 也已进入 MatrixReport。
iOS simulator/Android runner、移动 matrix control driver 也已落地；preview desktop/iOS/Android
构建现在会在 source-project 的 target-specific output root 上取得跨进程锁，串行保护同一输出根的
输出变更；desktop preview、iOS simulator live preview 与 Android default-debug live preview 已能
基于独立 verified artifact manifest 命中并跳过相应构建步骤；matrix control scenario cell 现在
保留完整 `CheckReport`，包括 steps、证据和 cleanup。
现阶段仍未完成
preview/移动 capture-only 路径的完整 scenario 语义/输入验收、MCP、
复现包或性能验证闭环。

35 项工作包更新为 **1 done、21 in_progress、13 planned、0 in_review**。
O03 保留已有 `done`；S01/M01/M03/T06 从过时的 `planned` 改为 `in_progress`；
F01/T01/P01/T02/T03 因仍缺工作包要求的实现或验收，从 `in_review` 校正为 `in_progress`。
这不是完成百分比，也不表示这些任务的已有实现被撤回。

| 门槛 | 已有进展 | 未收口部分 |
| --- | --- | --- |
| G0 | headless 基线、target-aware doctor、macOS 观察 PoC | 完整平台基线、版本解析/兼容规则、PoC 未支持的能力 |
| G1 | 窗口/心跳、资源 ACK、产物库、macOS best-effort observe | v1 兼容、真实历史升级、窗口实际环境、same-scene/present 与完整故障验收 |
| G2 | schema、三个 preview、query/diff、动作、check/baseline、租约/构建键、单场景及 matrix frozen inputs、per-cell context/target BuildKey、target-specific output layout/preview output-root lock、matrix cell CheckReport、普通 build/run 与 desktop/iOS simulator/Android default-debug preview 的 BuildKey coordinator、reference-aware caller-cancel 与 owned process termination、显式 coordinator `Cancelled`/`Partial` 终态、follower 取消和 superseded leader process-tree 终止、cleanup/fixture identity hardening | partial artifact 的消费/恢复契约、语义激活、真实环境适配、heartbeat/fencing、移动 capture-only 路径的完整 scenario steps/cleanup、真实连续场景验收及 MCP |
| G3 | 两种移动 runner、进程证据、matrix admission/并行调度/control/native capture | 完整三端同快照矩阵、可靠日志归属、设备重连、repro 和 L2/L3 CI |
| G4 | 普通构建缓存复用、desktop/iOS simulator/Android default-debug live preview verified cache hit 和显式清理 | 缓存输入遗漏、iOS physical/Android custom or signing-sensitive preview cache hit、增量索引、共享构建/预热、性能指标/预算和 Agent 基准 |

## 2. 相对上次审计的新合并

| 提交 | 新进展 | 判定边界 |
| --- | --- | --- |
| `bb5c10f` | Android runner adapter 接入普通运行 | 不能再写“Android adapter 未实现” |
| `8f1a4ce`、`31e403e`、`ea37ddf` | Android 启动身份、iOS 进程探测、移动 fault evidence 分类 | 不等于完整真实设备故障矩阵或持续日志归属已通过 |
| `942fec4`、`b41aee5`、`87e6af9` | matrix plan/scheduler、执行 adapter、target/scenario/host/ABI/toolchain admission | 不可用 cell 保留原因，required/optional 分开汇总 |
| `54069f1`、`301912d`、`42cdfba` | 并行 executor、资源锁、mobile lifecycle、`gpui check --matrix` | 同一 supervisor 的资源锁不替代主机设备租约；尚未共用冻结快照 |
| `b405cca`（#145） | 移动 matrix 通过 preview/control 执行场景并接 native capture | 单场景 `check --target ios/android` 仍不支持；移动入口是 matrix |
| `14f81fe`（#146） | 单场景和 matrix preview 专属 session key、定向发现；Android preview 元数据/fixture 接入 | 修复旧会话误绑定路径；不同时修复桌面进程清理或 fixture hash |
| `a1395d3`（#147） | `CheckReport.context` 保存 reset generation、environment、uncontrolled inputs | 单场景 JSON 可见；matrix 汇总尚未传递 context，报告值也不等于实际环境已受控 |
| `1256bb3`（#149） | BuildKey 环境 allowlist 纳入 `CARGO_ENCODED_RUSTFLAGS`，并增加 hash 回归测试 | 修复该已确认的缓存键遗漏；不等于所有 build.rs/Gradle/NDK/Xcode 隐藏输入或整个 M04/T06 已完成 |
| `5b63656`（#151） | desktop check preview 使用 process-group/Windows Job Object 归属，cleanup/Drop 终止整个 owned process tree；新增后代管道回归测试 | 修复 supervisor-only cleanup 路径；真实 GPUI check 的完整 GUI 进程验收、fixture hash 和其他 S04 门槛仍未完成 |
| `3e8b874`（#153） | reset 重新计算当前 fixture SHA-256，新的 `scenario_ready` 和单场景 `CheckReport` 使用 runtime fixture identity | 修复 reset 后报告仍保留启动 hash 的路径；真实 GUI reset probe、matrix report 传播和冻结构建仍未完成 |
| `12b1338`（#155） | 单场景 desktop check 通过 `desktop_build_plan` 创建并校验冻结 workspace snapshot，从 snapshot root 启动 preview；`CheckContext` 暴露 snapshot hash 和 BuildKey | matrix check 保持旧路径，尚未共享同一冻结 snapshot；local `build.rs` 等未建模输入会让严格 check 直接不可用，不回退到可变目录 |
| `97406bb`（#157） | matrix 在 admission 前创建并重核验一个 workspace snapshot，从快照重读 scenario/matrix 配置，所有 cell preview 复用同一 runtime root/hash；已知 local `build.rs` 输入继续拒绝严格执行 | target-specific BuildKey/构建输出编排和 MatrixReport context/完整步骤传播仍未接入；真实三端矩阵仍未验收 |
| `85e9c0d`（#159） | matrix cell execution 现在将 `CheckReport.context` 传入 `MatrixReport`，保留 reset generation、environment、uncontrolled inputs 和 shared snapshot hash | MatrixReport 仍未保留完整 steps/cleanup 详情；target-specific BuildKey/构建输出编排和真实三端矩阵仍未验收 |
| `ae9ae94`（#161） | matrix 按 ready target 从共享 frozen root 计算 desktop、iOS simulator、Android ABI-specific BuildKey，并写入每个 cell 的 `CheckContext.build_key` | 只提供 key evidence；target-specific Cargo/Gradle/Xcode 输出编排、共享构建/在途任务合并、完整 steps/cleanup 和真实三端矩阵仍未验收 |
| `4f35c95`（#163） | matrix preview 将 target BuildKey 绑定到 source-project 的 Cargo target、iOS DerivedData、Android JNI/Gradle 输出布局；同一 target BuildKey 的 cell 通过 matrix resource 串行 | 不覆盖跨命令共享构建/在途任务合并、cache hit/coalescing、完整 steps/cleanup 或真实三端矩阵验收 |
| `cca46b3`（#165） | preview desktop/iOS/Android builder 在 source-project 的 target-specific output root 取得 `BuildOutputLock`；root 路径随受控环境传入 preview，并增加跨进程释放回归测试 | 只保护进入该 preview lock 路径的输出变更；不提供 cache hit、manifest 复用、跨命令构建所有权或在途任务 coalescing |
| `5e5031a`（#167） | matrix control scenario cell 将完整 `CheckReport` 保留到 `MatrixCellResult`，JSON 包含 steps、action/assertion/capture evidence、primary error、cleanup 和 context，并增加反序列化 round-trip 回归测试 | 不为 admission-unavailable 或 capture-only mobile lifecycle cell 伪造 scenario report；不覆盖真实三端连续验收或跨命令构建所有权 |
| `7283614`（#169） | desktop preview 将 BuildKey hash 通过受控环境传入，在 output root 的独立 preview manifest 经过 platform/key/file/content hash 验证且只有一个可执行文件时跳过 Cargo；manifest 缺失、损坏或歧义回退构建 | 仅覆盖 desktop preview；不覆盖 iOS/Android preview cache hit、跨命令构建所有权、在途任务 coalescing 或隐藏输入建模 |
| `f6801e9`（#171） | iOS live preview 将 BuildKey hash 通过受控环境传入，在 output root 发布并校验独立 iOS `.app` manifest；simulator 命中时跳过 rustup/Cargo/XcodeGen/`xcodebuild`，physical device 仍强制重建 | 真机签名 identity/Provisioning Profile 等输入仍未建模；不覆盖 Android preview、跨命令构建所有权、在途任务 coalescing 或隐藏输入建模 |
| `0c2771e`（#173） | Android live preview 将 cache policy、ABI 和 default debug keystore hash 传入 preview；在 output root 校验 JNI staging 与 debug APK 输出的完整 manifest，命中时跳过 rustup/cargo-ndk/Gradle | release/custom/sensitive signing、keystore 变化、工具链身份不可读或隐藏输入会 bypass；不覆盖跨命令构建所有权、在途任务 coalescing 或完整设备验收 |
| `14339b5`（#175） | `BuildOutputLock` 在取得 OS 锁后原子写入 `.build-owner.json`，记录 schema、owner、PID、开始时间、状态和可选 BuildKey；释放时仅删除 owner_id 匹配的记录，stale 记录由下一个持锁者覆盖，cache size 统计排除该元数据 | 这是跨进程 ownership 证据，不是 coordinator、心跳/fencing 状态机、subscriber/取消引用或同 key 在途任务合并；OS 锁仍是活跃性唯一权威 |
| `5f6859d`（#177） | 普通 desktop/iOS/Android build/run 的可复用 BuildKey 通过持久 coordinator record 选举单一 leader；同 key follower 持有订阅文件 OS 锁、等待终态并复核完整 artifact manifest，失败可共享、leader 消失可接管，cache clean 避开活跃 subscriber | 尚未接入 live preview/check 构建；无调用者取消引用计数、无订阅者归零后的终止策略、无 heartbeat/fencing/partial 状态机；含未建模输入的路径仍不共享 |
| `e6aeb62`（#189） | subscriber 文件名绑定 attempt identity；active count 以 OS lock probe 判定，不读取 locked JSON；failed attempt sharing 按 attempt 隔离 | active count 仍不是完整 last-reference cancellation/state machine；无 heartbeat/fencing |
| `f4e10c2`（#190） | 记录 subscriber identity/count 状态与验证边界 | 状态文档，不增加运行时行为 |
| `16cf04b`（#191） | preview coordinator 在 leader revision superseded 时终止 owned process tree 并发布 retryable terminal marker；follower 释放旧引用后重新竞争；state lock 串行化注册、计数、record 与 cache-clean subscriber 检查；follower 单独取消不停止 leader | 仍无 `cancelled`/`partial` 状态或完整 last-reference policy；current leader 即使没有 follower 仍可完成自己的构建；无 heartbeat/fencing |
| `4b1a309`（#193） | coordinator 新增 caller-cancel reason；leader 在同一 state lock 内释放自身引用并统计剩余 subscribers，有 follower 时保留 terminal result，无 follower 时发布 retryable cancellation marker 并允许下一 leader；superseded reason 保持原有重建语义 | live preview/check 尚未接入 caller-cancel API；无自动 cooperative process termination、独立 `cancelled`/`partial` state 或 heartbeat/fencing |
| `3364c58`（#195） | desktop、iOS simulator、Android default-debug preview 安装 leader control 到 owned process loop；最后引用 caller-cancel 会终止 owned process tree 并发布 marker，有 follower 时 leader 脱离但共享构建继续；superseded 仍重建旧 attempt | 仍无独立 `cancelled`/`partial` state、heartbeat/fencing、physical/signing-sensitive preview 共享或完整真实设备验收 |
| `f8a95ce`（#197） | `BuildCoordinatorState::Cancelled` 明确表示无剩余 subscriber 的 caller-cancel；follower 放弃旧 attempt 并重新竞争，普通失败仍为 `Failed`，superseded 仍是 retryable `Failed` marker | 尚无 `Partial` 终态/部分产物消费契约、heartbeat/fencing、physical/signing-sensitive preview 共享或完整真实设备验收 |
| `0e05087`（#199） | 增加 `BuildCoordinatorState::Partial`；精确 partial marker 保留不完整输出诊断，但不验证/共享/命中该输出，旧 attempt 的 follower 放弃并重新竞争；error chain 支持 contextual marker | partial marker 需调用方显式返回；没有自动 partial 检测、可消费部分 manifest 或恢复机制，heartbeat/fencing、physical/signing-sensitive preview 共享及完整真实设备验收仍未完成 |

代码入口：[check](../../src/commands/check.rs)、[matrix admission](../../src/runner/matrix_admission.rs)、
[matrix executor](../../src/runner/matrix_executor.rs)、[mobile lifecycle adapter](../../src/runner/mobile_matrix.rs)、
[iOS runner](../../src/runner/ios.rs)、[Android runner](../../src/runner/android.rs)、
[scenario executor](../../src/scenario/executor.rs)。CLI 的移动场景路径复用 control runner；
独立 mobile lifecycle adapter 的契约测试不能替代该 CLI 调用链的设备验收。

## 3. 全部工作包

| ID | 状态 | 已实现 / 剩余边界 |
| --- | --- | --- |
| F01 | in_progress | 三种 fixture、单调 span、有界日志、10 预热/30 测量 headless 驱动器；缺完整 UI/native 故障及索引/缓存对照 |
| T01 | in_progress | target-aware JSON doctor、required/optional、超时；畸形版本仍可 pass，SDK/JDK/AGP/MSRV 规则未齐 |
| P01 | in_progress | 有真实 macOS PoC；scene readback、a11y 激活及完整帧/GPU/故障证据仍有限 |
| T02 | in_progress | 模板 manifest、嵌入内容摘要、增平台保护；真实历史内容的稳定取得仍不贯通 |
| F02 | in_progress | 独立 protocol crate、v2、请求关联和 feature 边界；无在线 v1 兼容/事件投影，runtime 仍在模板内 |
| T03 | in_progress | 保守 B/L/N 整文件计划和冲突保护；缺结构化合并及完整历史基线识别 |
| T04 | in_progress | 锁、journal、备份、多文件替换/恢复和并发保护；跨真实发行版升级及升级后的编译验证未齐 |
| O01 | in_progress | 窗口身份、UI heartbeat、迟到/关闭处理；实际 resize/DPI/前台状态和完整故障矩阵未齐 |
| O02 | in_progress | 资源事务、删除、hash、分层 ACK、重连对账；不证明 GPU 呈现，真实完整变体未齐 |
| O03 | done | 产物分块/hash、原子发布、配额、pin/过期和导出；仅覆盖产物库 |
| O04 | in_progress | observe、操作生命周期、build request、macOS window capture；best_effort，不承诺 same_scene/present |
| O05 | in_progress | debug-a11y 快照和显式 logical_id bridge；受激活/provider 限制，非通用完整语义树 |
| O06 | in_progress | observation-bound query/diff、分页和预算；真实大树/虚拟化与单节点 artifact 验收未齐 |
| S01 | in_progress | 静态 schema、fixture 路径/hash、registry 校验已实现；自定义 schema 与完整环境确定性未齐 |
| S02 | in_progress | Counter/LoginForm/VirtualList preview/reset；Android preview 已接入，真实环境适配仍有缺口 |
| S03 | in_progress | click/type/key/scroll、正常事件路径、owner/scope 和 unknown 语义；真实输入/遮挡/污染及持久幂等验收未齐 |
| M03 | in_progress | 主机 OS 锁、owner/fencing、heartbeat，run/live/capture 和移动 matrix 已接入；重连和真实竞争矩阵未齐 |
| M04 | in_progress | 稳定扫描、外部 Cargo path root、普通三端构建冻结/输出隔离/manifest；单场景及 matrix 已消费共享冻结 snapshot/target BuildKey、绑定输出布局并锁定 preview 输出根，普通 desktop/iOS/Android build/run 已接入 BuildKey coordinator，desktop/iOS simulator/Android default-debug live preview 已接入 verified manifest 命中及 caller-cancel/显式 Cancelled/Partial 状态，BuildKey output ownership record 已接入；partial 输出消费/恢复、heartbeat/fencing、隐藏输入建模和其余 preview 路径仍未齐 |
| S04 | in_progress | executor、desktop check、单场景及 matrix frozen inputs、baseline/diff/approve、移动 matrix 场景 driver、per-cell context/BuildKey/output layout/preview output lock/完整 CheckReport、owned process-tree/fixture identity cleanup、preview coordinator caller-cancel 与显式 Cancelled/Partial 状态；移动 capture-only 语义/输入、环境及三夹具各 20 次真实验收未齐 |
| A01 | planned | CLI/control 可复用；无 MCP 只读适配、JSON-RPC server 或独立 Agent service |
| A02 | planned | action/operation/check 可复用；无 MCP 动作/取消/owner 适配 |
| A03 | planned | pin/manifest/registry 可复用；无 context 命令、版本知识索引或工作流包 |
| S05 | planned | 无类型化热参数、overlay revision、撤销/固化及收益实验 |
| M01 | in_progress | iOS/Android adapter、capture/log snapshot、进程身份与 fault evidence；缺完整日志归属、设备故障和 UI 变体验收 |
| M02 | in_progress | 配置展开、admission、并行/资源锁、matrix CLI、移动 control/native capture、共享 frozen snapshot/target output lock、control scenario cell 完整报告；普通 build/run 已有跨命令构建所有权，仍缺 preview/check 接入、移动完整 scenario 证据和三端矩阵 |
| M05 | planned | 无 repro export/inspect/run、脱敏和干净环境重放 |
| Q01 | planned | 已有三 OS CLI/macOS 模板/Android 宿主 CI；尚无该工作包的完整真实 GUI/设备 L2/L3 门禁 |
| G01 | planned | supervisor 计时已有；无应用 layout/paint/frame/CPU/GPU 指标与开销验收 |
| G02 | planned | 无 perf 执行器、统计/可比性和性能预算判定 |
| T05 | planned | watcher/全量内容扫描已有；无增量输入索引及大项目对照 |
| T06 | in_progress | desktop/iOS simulator/Android default-debug 缓存与 cache clean，普通 build/run 及受支持的 live preview 已接入同 key coordinator、verified manifest 和 caller-cancel/显式 Cancelled/Partial 状态，BuildKey output ownership record 已落地；Partial 仅为不可复用的诊断终态；heartbeat/fencing、输入遗漏、iOS physical/Android custom or signing-sensitive preview 和预热未实现 |
| Q02 | planned | 仅有 12 项任务设计；无可执行评分器和固定预算对照实验 |
| G03 | planned | 无 GPU capture/analysis provider 闭环；可选 |
| M06 | planned | 无远程 runner、传输和断线恢复；可选 |
| A04 | planned | JSON/diff 已有；无静态审阅器或 MCP Apps；可选 |

## 4. 旧审计问题的当前状态

以下编号延续原报告的七项问题。本轮使用当前二进制/模板和隔离夹具重新核查，
问题 1–4 的旧触发已有局部修复，其余三项仍可复现；不将局部修复扩大为整个工作包完成。

| 编号 | 结论 | 代码与影响 |
| --- | --- | --- |
| 1 缓存环境键 | 已修复该已确认遗漏 | #149 在 [build_inputs.rs](../../src/runner/build_inputs.rs) 的 allowlist 加入 `CARGO_ENCODED_RUSTFLAGS`，并用不同 encoded flag 值证明环境 hash 改变；其他未建模输入仍不因此解决 |
| 2 check 误绑定旧 preview | 已修复该路径 | #146 为单场景和 matrix 注入 session key；[control](../../src/devserver/control.rs) 按 target suffix 定向发现；复测旧 preview 未收到 check reset 且仍存活 |
| 3 桌面 cleanup 假成功 | 已修复 supervisor-only 路径 | #151 让 [check cleanup/Drop](../../src/commands/check.rs) 通过 `OwnedChild` 使用 Unix process group/Windows Job Object，并以 descendant-held pipe 回归测试证明后代随终止关闭；真实 GPUI check 的完整 GUI 进程探针仍未重跑 |
| 4 fixture 变化但 hash 不变 | 已修复 reset/report 路径 | #153 让 [preview reset](../../templates/app/src/previews.rs) 按当前 fixture bytes 重算 SHA-256，新的 ready 事件和 `CheckReport.fixture_hash` 使用 runtime identity；真实 GUI probe 与 matrix 汇总传播仍未完成 |
| 5 v1 在线兼容 | 未解决 | [app channel](../../src/devserver/app_channel.rs) 拒绝 proto 1，[control](../../src/devserver/control.rs) 拒绝 schema 1 |
| 6 真实历史模板升级 | 未解决 | [模板基线解析](../../src/template.rs) 仍缺不可变历史内容；旧 T02 项目会遇到 `baseline_unavailable` |
| 7 doctor 畸形版本 | 未解决 | [probe](../../src/toolchain/probe.rs) 仍按命令退出状态判定，缺版本解析 |

另外两条旧缺口继续有效，并补记本轮确认的 matrix 报告边界：

- 单场景 desktop `check` 已通过 `desktop_build_plan` 创建并重核验 FrozenInputs snapshot；
  matrix check 也在 admission 前创建一个严格 snapshot，所有 cell 从同一 runtime root 启动
  preview，并把原项目 root 仅用于 baseline/diff 和移动 artifact 输出。已知 local `build.rs`
  等未建模输入会让严格 check/matrix 直接不可用，不回退到可变目录；matrix target-specific
  BuildKey 已从 shared snapshot 计算并绑定到 source-project output layout；普通 build/run 的跨命令
  coordinator 已接入，但 check/matrix preview 仍未消费该 coordinator，cache coalescing 和在途任务
  所有权在 preview/check 路径仍未接入。
- [桌面模板](../../templates/desktop/src/main.rs) 仍固定 Light 主题及窗口注册尺寸；
  `ready_environment` 中 theme/locale/clock/seed 来自配置。报告保留这些值改善了追踪性，
  不能代替真实 viewport/DPI、环境控制、字体/backend 和语义 provider 的验证。
- matrix CLI 现在把内部 `CheckReport` 传到 control scenario 的每个 `MatrixCellResult`，因此
  matrix JSON 可见 reset generation、environment、uncontrolled inputs、shared snapshot hash、
  完整 steps/证据和 cleanup；admission-unavailable cell 与 capture-only mobile lifecycle cell
  保持没有 scenario report，不用空报告冒充执行证据。该切片关闭了摘要投影缺口，但仍不能扩大
  为真实三端 matrix execution evidence 或实际环境已受控。

## 5. 验证记录与证据范围

以下“上一轮审计”记录针对 `5f6859d`，作为历史证据保留；本地原始输出、隔离探针源码及 JSON 保存在
`artifacts/progress-audit-2026-09-29/`（忽略的本机产物目录，不是已发布验收证据）。
测试独立设置 `GPUI_DEVICE_LEASE_DIR`，避免与其他 worktree 的租约测试互相影响。

| 检查 | 本轮结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --workspace --locked` | 348 个单元测试及全部集成/协议测试通过，0 failed |
| `cargo build --locked` | 通过 |
| `cargo x check-design-docs`、`git diff --check` | 通过；仅验证文档/示例一致性 |
| 两份 35 项状态表逐项比对 | 相同；1 done、21 in_progress、13 planned |
| PR #151 CI | required checks 全部通过；首次 Windows 并发测试波动重跑后通过，三 OS check、desktop-template、android-template、baseline-driver 均通过 |
| PR #155 CI | required checks 全部通过；三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #155](https://github.com/kingsword09/gpui-cli/pull/155) |
| PR #157 CI | required checks 全部通过；三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #157](https://github.com/kingsword09/gpui-cli/pull/157) |
| PR #159 CI | required checks 全部通过；macOS capture helper 初次超时后重跑通过，三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #159](https://github.com/kingsword09/gpui-cli/pull/159) |
| PR #161 CI | required checks 全部通过；三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #161](https://github.com/kingsword09/gpui-cli/pull/161) |
| PR #163 CI | required checks 全部通过；Windows pointer-dispatch 既有测试首次超时后重跑通过，三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #163](https://github.com/kingsword09/gpui-cli/pull/163) |
| PR #165 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #165](https://github.com/kingsword09/gpui-cli/pull/165) |
| PR #167 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #167](https://github.com/kingsword09/gpui-cli/pull/167) |
| PR #169 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #169](https://github.com/kingsword09/gpui-cli/pull/169) |
| PR #171 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #171](https://github.com/kingsword09/gpui-cli/pull/171) |
| PR #173 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；macOS capture helper deadline 初次波动后重跑通过；无 release/tag | [PR #173](https://github.com/kingsword09/gpui-cli/pull/173) |
| PR #175 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #175](https://github.com/kingsword09/gpui-cli/pull/175) |
| PR #177 CI | required checks 全部通过；Android template 先修复 follower cache-hit 诊断契约后重跑，Windows live-feedback 时序波动重跑通过；三 OS check、desktop-template、android-template、baseline-driver 最终均通过；无 release/tag | [PR #177](https://github.com/kingsword09/gpui-cli/pull/177) |

本次接续到 `0e05087` 的增量证据：

| 检查/合并 | 结果 |
| --- | --- |
| 本地运行时验证（PR #197） | 359 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #195/#196 | preview owned-process cancellation 接线及对应状态记录均已 squash 合并；无发布/tag |
| PR #197 CI | PR 与 push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 和文档门槛均通过；push 的 Windows 时序 job 在重跑失败 job 后通过 |
| PR #197 合并 | squash merge `f8a95ce`；无发布/tag |
| 本地运行时验证（PR #199） | 362 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #199 CI | PR 与 push 两套 CI 最终全通过；PR 首轮 Windows 既有 coordinator 时序测试失败后仅重跑失败 job，Windows 和 macOS 均通过，Linux、两类模板和 baseline-driver 通过 |
| PR #199 合并 | squash merge `0e05087`；无发布/tag |

七项探针的关键结果如下。这些是无 GPU 的边界复现，不是完整 UI 场景验收：

| 探针 | 观察结果 | 本机记录 |
| --- | --- | --- |
| encoded Rust flags | 修复前探针曾在改编译参数后仍 cache hit；#149 已将变量纳入 allowlist，回归测试证明 encoded flag 改变 hash；尚未重跑合并后的 end-to-end CLI probe | `cli-probes.json`、[M04 encoded Rust flags](../experiments/M04-cargo-encoded-rustflags-2026-09-29.md) |
| check 会话归属 | 旧 preview 收到的 reset 数为 0，旧 preview 仍运行 | `check-probes.json` |
| check 清理 | 旧探针记录了 supervisor-only cleanup 的问题；#151 已改用 process group/Job Object，并用后代持有管道回归测试验证终止传播；尚未重跑完整 GUI check cleanup probe | `check-probes.json`、[S04 process-tree cleanup](../experiments/S04-process-tree-cleanup-2026-09-29.md) |
| fixture 身份 | 修复前探针记录初值 0→42、generation 1→2，报告 hash 仍为初始值；#153 已接入 runtime hash 刷新和报告传播，尚未重跑完整 GUI probe | `runtime-probes.json`、[S04 fixture identity](../experiments/S04-fixture-hash-2026-09-29.md) |
| 单场景 frozen check | #155 已让 desktop 单场景从重核验后的 workspace snapshot 读取 scenario/fixture，并在 context 中保留 snapshot hash/BuildKey；matrix 未纳入本切片 | [S04 frozen single check](../experiments/S04-frozen-single-check-2026-09-29.md) |
| matrix frozen inputs | #157 创建 shared snapshot，#159 写入 per-cell context，#161 写入 target-specific BuildKey，#163 绑定 source-project output layout 并串行同 key cell，#165 在 preview builder 取得该 output root 的跨进程锁，#167 保留 control scenario cell 的完整 CheckReport；仍未声称跨命令共享构建或真实三端 evidence | [S04 frozen matrix snapshot](../experiments/S04-frozen-matrix-snapshot-2026-09-29.md)、[S04 matrix context](../experiments/S04-matrix-context-2026-09-29.md)、[S04 matrix target BuildKey](../experiments/S04-matrix-target-build-key-2026-09-29.md)、[S04 matrix build output layout](../experiments/S04-matrix-build-output-layout-2026-09-29.md)、[S04 preview build output lock](../experiments/S04-preview-build-output-lock-2026-09-29.md)、[S04 matrix cell reports](../experiments/S04-matrix-cell-reports-2026-09-29.md) |
| preview output lock | preview desktop/iOS/Android build 在 source-project target-specific root 上通过持久 lock file 做跨进程排他；锁 guard 覆盖 preview build 过程并在进程退出/崩溃时由 OS 释放；只验证 lock 阻塞和 guard drop 释放，不声称命中或合并构建 | [S04 preview build output lock](../experiments/S04-preview-build-output-lock-2026-09-29.md) |
| matrix cell report | control scenario cell 的 `MatrixCellResult.check_report` 保留完整 steps、证据、primary error、cleanup 和 context，并通过 JSON round-trip 验证；unavailable/capture-only cell 不生成伪报告 | [S04 matrix cell reports](../experiments/S04-matrix-cell-reports-2026-09-29.md) |
| desktop preview cache hit | preview 专用 manifest 绑定 desktop platform/BuildKey hash，逐文件验证内容并要求唯一 Cargo 可执行文件；命中跳过 Cargo，任何验证失败都 miss 并回退正常构建 | [T06 desktop preview cache hit](../experiments/T06-desktop-preview-cache-hit-2026-09-29.md) |
| iOS simulator live preview cache hit | preview 专用 manifest 绑定 iOS platform/BuildKey hash，逐文件验证 `.app` 内容并要求根正是当前 simulator bundle；命中跳过 rustup/Cargo/XcodeGen/`xcodebuild`，physical device 不命中 | [T06 iOS preview cache hit](../experiments/T06-ios-preview-cache-hit-2026-09-29.md) |
| Android default-debug live preview cache hit | preview 专用 manifest 绑定 Android platform/BuildKey/ABI，逐文件验证 JNI staging 与 debug APK 输出；default debug keystore 和 cache policy 仍有效时命中并跳过 rustup/cargo-ndk/Gradle | [T06 Android preview cache hit](../experiments/T06-android-preview-cache-hit-2026-09-29.md) |
| BuildKey output ownership | `BuildOutputLock` 持有 OS 锁后原子发布 `.build-owner.json`；owner_id 匹配时才删除，stale 记录在下一次成功加锁后覆盖，cache clean 不把 owner record 计入输出大小；不以 record 推断锁活跃性 | [S04 BuildKey output ownership](../experiments/S04-build-output-ownership-2026-09-29.md) |
| BuildKey coordinator | 普通 build/run 与 desktop、iOS simulator、Android default-debug live preview/check 均使用独立 attempt 类型和平台 manifest verifier；preview follower 可在 superseded 时释放 subscriber，leader 消失可接管、失败/输入 supersession 可共享；release/custom-signing、physical device 和 cache-disabled 路径仍不共享 | [S04 BuildKey coordinator](../experiments/S04-build-coordinator-2026-09-29.md)、[S04 desktop preview coordinator](../experiments/S04-preview-build-coordinator-2026-09-30.md)、[S04 iOS preview coordinator](../experiments/S04-ios-preview-build-coordinator-2026-09-30.md)、[S04 Android preview coordinator](../experiments/S04-android-preview-build-coordinator-2026-09-30.md)、[S04 preview coordinator cancellation](../experiments/S04-preview-coordinator-cancellation-2026-09-30.md) |
| Leader subscriber reference | coordinator leader 从发布 `building` 状态前持有 OS-locked subscriber，终态发布并返回后释放；failed attempt 在 leader 返回前仍被视为有活跃引用，follower 取消只释放自身引用 | [S04 leader subscriber reference](../experiments/S04-leader-subscriber-reference-2026-09-30.md) |
| Subscriber identity/count | subscriber 文件名绑定 attempt 的安全编码，active count 通过 OS lock probe 判定，不读取 locked JSON；failed-attempt sharing 按 attempt 过滤 | [S04 subscriber identity/count](../experiments/S04-subscriber-identity-count-2026-09-30.md) |
| v1 兼容 | app proto 1 为 `unsupported_version`；control schema 1 为 `invalid_schema`；schema 2 可用 | `cli-probes.json` |
| 历史升级 | 复制上次用 `3df7a6c` CLI 生成的未修改项目，执行 `upgrade plan --to agent-native-v1-draft --json`，仍退出 1、`baseline_unavailable` | `upgrade-probe.json`、`old-t02-upgrade.json` |
| doctor 版本 | shim 输出不可解析版本但退出 0，doctor 仍 overall=pass | `runtime-probes.json` |

fixture 探针直接编译当前 `previews.rs`，只用最小 env/report bridge 隔离验证 hash；
历史升级复用旧项目夹具，本轮没有重新编译旧 CLI。探针返回的预期失败不能统计为
工作包验收通过；原始脚本的退出码也不能替代逐项结果核对。

已有 Android capture-only 真实探针记录见
[M02 实验](../experiments/M02-matrix-contract-2026-09-28.md)：Counter 安装/启动/control/heartbeat/
设备 PNG capture 成功，1280×2856；同设备 semantics-required 场景为 unavailable。
PNG 仅有临时路径和记录 hash，未形成持久 CI 产物。本轮没有重跑 GUI 或设备验收。

## 6. 后续接续顺序

1. 在共享 snapshot/context/BuildKey/output layout/preview lock/ownership/coordinator/完整 cell report 的现有基础上，实现 coordinator owner heartbeat/fencing，并验证 stale owner、OS lock 和 subscriber reference 的交互；之后补齐 iOS physical signing 输入、Android custom/release/signing-sensitive preview、移动完整 scenario 证据以及真实环境身份/viewport/DPI 证据。
2. 补齐 v1 在线兼容、不可变历史模板基线和 doctor 版本解析；保留现有拒绝/降级边界。
3. 完成 macOS 三夹具各连续 20 次及故障变体，再收口移动语义/输入和完整三端矩阵。
4. 按依赖继续 MCP、repro、L2/L3 CI；性能、索引/预热、Agent 基准按各自验收推进。

每次新合并先比较本文代码基线，再更新相关行、问题结论和验证记录。不得继续引用
`a6aa685` 的“matrix/Android adapter 未实现”或把旧 320 项测试当作新提交的检查结果。
