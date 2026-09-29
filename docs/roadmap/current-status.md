# 主分支进度与接续记录

更新日期：2026-09-29（Asia/Shanghai）。核查代码：`5b63656`（PR #151 squash merge）。
本轮 fetch 后，本地 `main` 与 `origin/main` 均指向该提交。后续提交须重新核对，本文不是动态状态。

本文接续 2026-09-28 对 `a6aa685` 的审计，替代其“当前进度”结论；旧报告保留为历史证据。
任务状态以[实施清单](implementation-backlog.md)为准，完成标准以[验收矩阵](acceptance-matrix.md)为准。
专项设计中的目标接口和历史实验记录不能直接当作当前实现或全平台验收。

## 1. 当前结论

基础 CLI/Live 已有实现，macOS 窗口观察有限可用；场景、输入、check、视觉基线和本地 matrix
已接入代码。iOS simulator/Android runner、移动 matrix control driver 也已落地。
现阶段仍未完成同一冻结快照的场景矩阵、完整移动语义/输入验收、MCP、复现包或性能验证闭环。

35 项工作包更新为 **1 done、21 in_progress、13 planned、0 in_review**。
O03 保留已有 `done`；S01/M01/M03/T06 从过时的 `planned` 改为 `in_progress`；
F01/T01/P01/T02/T03 因仍缺工作包要求的实现或验收，从 `in_review` 校正为 `in_progress`。
这不是完成百分比，也不表示这些任务的已有实现被撤回。

| 门槛 | 已有进展 | 未收口部分 |
| --- | --- | --- |
| G0 | headless 基线、target-aware doctor、macOS 观察 PoC | 完整平台基线、版本解析/兼容规则、PoC 未支持的能力 |
| G1 | 窗口/心跳、资源 ACK、产物库、macOS best-effort observe | v1 兼容、真实历史升级、窗口实际环境、same-scene/present 与完整故障验收 |
| G2 | schema、三个 preview、query/diff、动作、check/baseline、租约/构建键 | 语义激活、清理/fixture 身份、冻结 check、真实连续场景验收及 MCP |
| G3 | 两种移动 runner、进程证据、matrix admission/并行调度/control/native capture | 完整三端同快照矩阵、可靠日志归属、设备重连、repro 和 L2/L3 CI |
| G4 | 普通构建的缓存复用和显式清理 | 缓存输入遗漏、增量索引、共享构建/预热、性能指标/预算和 Agent 基准 |

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
| S02 | in_progress | Counter/LoginForm/VirtualList preview/reset；Android preview 已接入，fixture 身份及环境适配仍有缺口 |
| S03 | in_progress | click/type/key/scroll、正常事件路径、owner/scope 和 unknown 语义；真实输入/遮挡/污染及持久幂等验收未齐 |
| M03 | in_progress | 主机 OS 锁、owner/fencing、heartbeat，run/live/capture 和移动 matrix 已接入；重连和真实竞争矩阵未齐 |
| M04 | in_progress | 稳定扫描、外部 Cargo path root、普通三端构建冻结/输出隔离/manifest；`CARGO_ENCODED_RUSTFLAGS` 已纳入 key，check/matrix 未冻结且仍有未建模输入 |
| S04 | in_progress | executor、desktop check、baseline/diff/approve、移动 matrix 场景 driver、报告 context、owned process-tree cleanup；fixture/环境及三夹具各 20 次真实验收未齐 |
| A01 | planned | CLI/control 可复用；无 MCP 只读适配、JSON-RPC server 或独立 Agent service |
| A02 | planned | action/operation/check 可复用；无 MCP 动作/取消/owner 适配 |
| A03 | planned | pin/manifest/registry 可复用；无 context 命令、版本知识索引或工作流包 |
| S05 | planned | 无类型化热参数、overlay revision、撤销/固化及收益实验 |
| M01 | in_progress | iOS/Android adapter、capture/log snapshot、进程身份与 fault evidence；缺完整日志归属、设备故障和 UI 变体验收 |
| M02 | in_progress | 配置展开、admission、并行/资源锁、matrix CLI、移动 control/native capture；缺同快照构建、完整 cell 报告保留和三端矩阵 |
| M05 | planned | 无 repro export/inspect/run、脱敏和干净环境重放 |
| Q01 | planned | 已有三 OS CLI/macOS 模板/Android 宿主 CI；尚无该工作包的完整真实 GUI/设备 L2/L3 门禁 |
| G01 | planned | supervisor 计时已有；无应用 layout/paint/frame/CPU/GPU 指标与开销验收 |
| G02 | planned | 无 perf 执行器、统计/可比性和性能预算判定 |
| T05 | planned | watcher/全量内容扫描已有；无增量输入索引及大项目对照 |
| T06 | in_progress | desktop/iOS simulator/Android default-debug 缓存与 cache clean；输入键有遗漏，共享在途构建/取消/预热未实现 |
| Q02 | planned | 仅有 12 项任务设计；无可执行评分器和固定预算对照实验 |
| G03 | planned | 无 GPU capture/analysis provider 闭环；可选 |
| M06 | planned | 无远程 runner、传输和断线恢复；可选 |
| A04 | planned | JSON/diff 已有；无静态审阅器或 MCP Apps；可选 |

## 4. 旧审计问题的当前状态

以下编号延续原报告的七项问题。本轮使用当前二进制/模板和隔离夹具重新核查，
问题 1–3 的旧触发已有局部修复，其余四项仍可复现；不将局部修复扩大为整个工作包完成。

| 编号 | 结论 | 代码与影响 |
| --- | --- | --- |
| 1 缓存环境键 | 已修复该已确认遗漏 | #149 在 [build_inputs.rs](../../src/runner/build_inputs.rs) 的 allowlist 加入 `CARGO_ENCODED_RUSTFLAGS`，并用不同 encoded flag 值证明环境 hash 改变；其他未建模输入仍不因此解决 |
| 2 check 误绑定旧 preview | 已修复该路径 | #146 为单场景和 matrix 注入 session key；[control](../../src/devserver/control.rs) 按 target suffix 定向发现；复测旧 preview 未收到 check reset 且仍存活 |
| 3 桌面 cleanup 假成功 | 已修复 supervisor-only 路径 | #151 让 [check cleanup/Drop](../../src/commands/check.rs) 通过 `OwnedChild` 使用 Unix process group/Windows Job Object，并以 descendant-held pipe 回归测试证明后代随终止关闭；真实 GPUI check 的完整 GUI 进程探针仍未重跑 |
| 4 fixture 变化但 hash 不变 | 未解决 | [preview reset](../../templates/app/src/previews.rs) 重读文件但不重算 hash；#147 的 context 不修复该身份问题 |
| 5 v1 在线兼容 | 未解决 | [app channel](../../src/devserver/app_channel.rs) 拒绝 proto 1，[control](../../src/devserver/control.rs) 拒绝 schema 1 |
| 6 真实历史模板升级 | 未解决 | [模板基线解析](../../src/template.rs) 仍缺不可变历史内容；旧 T02 项目会遇到 `baseline_unavailable` |
| 7 doctor 畸形版本 | 未解决 | [probe](../../src/toolchain/probe.rs) 仍按命令退出状态判定，缺版本解析 |

另外两条旧缺口继续有效，并补记本轮确认的 matrix 报告边界：

- `check → preview → live` 仍在可变项目根执行构建；matrix 接受 `source_mode = "frozen"`
  不表示已创建或消费同一个冻结快照。普通 `build/run` 的冻结能力不能外推到这条路径。
- [桌面模板](../../templates/desktop/src/main.rs) 仍固定 Light 主题及窗口注册尺寸；
  `ready_environment` 中 theme/locale/clock/seed 来自配置。报告保留这些值改善了追踪性，
  不能代替真实 viewport/DPI、环境控制、字体/backend 和语义 provider 的验证。
- matrix CLI 把内部 `CheckReport` 转为 `MatrixCellExecution` 时只保留 status、primary error
  和 capture step 的 artifact IDs；没有传递 context、完整步骤与 cleanup 详情。
  因此 #147 不能表述为“matrix JSON 已包含 reset generation/实际环境”，该接线仍需补齐。

## 5. 验证记录与证据范围

本轮验证针对上述 `5b63656` 代码及本次文档更新。本地原始输出、隔离探针源码及 JSON 保存在
`artifacts/progress-audit-2026-09-29/`（忽略的本机产物目录，不是已发布验收证据）。
测试独立设置 `GPUI_DEVICE_LEASE_DIR`，避免与其他 worktree 的租约测试互相影响。

| 检查 | 本轮结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --workspace --locked` | 360 passed，0 failed |
| `cargo build --locked` | 通过 |
| `cargo x check-design-docs`、`git diff --check` | 通过；仅验证文档/示例一致性 |
| 两份 35 项状态表逐项比对 | 相同；1 done、21 in_progress、13 planned |
| PR #151 CI | required checks 全部通过；首次 Windows 并发测试波动重跑后通过，三 OS check、desktop-template、android-template、baseline-driver 均通过 |

七项探针的关键结果如下。这些是无 GPU 的边界复现，不是完整 UI 场景验收：

| 探针 | 观察结果 | 本机记录 |
| --- | --- | --- |
| encoded Rust flags | 修复前探针曾在改编译参数后仍 cache hit；#149 已将变量纳入 allowlist，回归测试证明 encoded flag 改变 hash；尚未重跑合并后的 end-to-end CLI probe | `cli-probes.json`、[M04 encoded Rust flags](../experiments/M04-cargo-encoded-rustflags-2026-09-29.md) |
| check 会话归属 | 旧 preview 收到的 reset 数为 0，旧 preview 仍运行 | `check-probes.json` |
| check 清理 | 旧探针记录了 supervisor-only cleanup 的问题；#151 已改用 process group/Job Object，并用后代持有管道回归测试验证终止传播；尚未重跑完整 GUI check cleanup probe | `check-probes.json`、[S04 process-tree cleanup](../experiments/S04-process-tree-cleanup-2026-09-29.md) |
| fixture 身份 | 初值 0→42、generation 1→2，报告 hash 仍为初始值 | `runtime-probes.json` |
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

1. 先处理缓存键、桌面清理、fixture/实际环境身份，再让 check/matrix 消费同一冻结快照。
2. 补齐 v1 在线兼容、不可变历史模板基线和 doctor 版本解析；保留现有拒绝/降级边界。
3. 完成 macOS 三夹具各连续 20 次及故障变体，再收口移动语义/输入和完整三端矩阵。
4. 按依赖继续 MCP、repro、L2/L3 CI；性能、索引/预热、Agent 基准按各自验收推进。

每次新合并先比较本文代码基线，再更新相关行、问题结论和验证记录。不得继续引用
`a6aa685` 的“matrix/Android adapter 未实现”或把旧 320 项测试当作新提交的检查结果。
