# 整体路线推进、收口与 Agent 接续

更新日期：2026-10-07（Asia/Shanghai）。执行基线：`9e6ef64`（PR #382 squash merge）；相较
实现核查 `852ddec` 和流程基线 `ac3db66`，已合并提交包含 T01 doctor、F01 baseline-driver、
iOS-safe backtrace 模板修复、F01/P-01 native-install-failure 证据和 T01 doctor CLI host-smoke。
当前分支为 `main`，工作区干净。T01/F01/P01 仍保持 `in_progress`；T01 host-smoke 已通过
design-doc、两套三平台 workspace/template/baseline-driver CI 并 squash merge。

## 1. 文档职责与纠偏原因

- [当前状态](current-status.md)：代码事实、历史证据、验证边界，不负责自动选择最近提交涉及的任务。
- [实施清单](implementation-backlog.md)：35 个父工作包的依赖、范围、正式状态。
- [验收矩阵](acceptance-matrix.md)：完整用例、平台层级及共享用例的任务责任；规范不等于执行结果。
- 本文：整体路线的自动选题循环、当前执行游标、收口/切换条件和接续记录；[AGENTS.md](../../AGENTS.md) 要求每个 Agent 遵守。

此前推进大量可合并切片，但早期兼容/版本/真实验收缺口未同步关闭。2026-10-04 至
`ac3db66` 的主线 source-changing commits 中，38 个涉及 `src`，37 个修改
`src/runner/build_inputs.rs`。这说明近期重心集中在缓存边界，不表示这些修复无价值，也不
证明其他工作包已完成。后续应优先减少一个工作包的剩余项，而非持续增加已实现字段或 gate。

还存在规划粒度问题：F01 的 P-02 同时包含 T05/T06 才负责的索引/缓存对照；S01 的 S-09
也包含 S02/S04 才负责的运行时环境。若每个父任务必须独自通过整个共享用例，就会形成
隐含的验收反向依赖。按矩阵第 1.4 节拆分责任子例，保留完整用例、所有证据和门槛，
不能通过删测试或把未执行的平台改成通过来消除这种依赖。

## 2. 整体目标与执行游标

默认目标是持续推进 G0–G4 核心路线，优先收口已有工作包，再实现依赖可满足的新任务。
不是“只实现 T01”，也不是“把 22 个 in_progress 同时开工”。每次只有一个实现焦点，
但一次整体推进可以顺序完成多个工作包；完成一个后自动选下一项，不以单个切片出口作为
整次执行的终点。用户指定有限范围时以该范围为准，可选支线不自动纳入核心目标。

| 字段 | 当前值 |
| --- | --- |
| 整体范围 | G0–G4 核心工作包，按依赖与收口优先持续推进 |
| 当前执行游标 | T01 · doctor CLI host-smoke 已在本机和 Linux/macOS/Windows `check` matrix 通过：生成 desktop-only 项目并执行 `doctor --json --target desktop`，schema v2、显式 target、required pass/可选 warning 和移动工具链隔离均核对。T01 仍缺真实 Linux/Windows 工具版本矩阵、x86/unknown ABI、physical-device 与未建模兼容边界；F01/P01 已回到等待/后续联合证据，P01 仍需 F01+T01 |
| 首批候选 | T01 继续处理 T-02/T-03 可执行 responsibility variants；优先复核真实工具版本/故障边界和可用设备变体，不越过 P01/F01 硬依赖 |
| 选题原则 | 可恢复的游标优先；否则按第 3 节规则选择，不能将示例任务当作永久主线 |
| 正式状态 | 仍为 1 done、22 in_progress、12 planned；选择或切换游标不等于晋升 |
| 不作为默认替代 | 用 T05/T06 优化、预热、API marker 或移动证据字段扩张绕开早期任务；它们依赖/范围满足后仍可按队列选择 |

### 本轮执行卡：F01 / P-01 iOS simulator native-install-failure

| 维度 | 本轮登记 |
| --- | --- |
| 实现缺口 | 已以内嵌 `vendor/backtrace-0.3.76` 兼容快照收口；cfg 差异限定为 macOS dyld library enumerator，许可证、上游 commit 和 PATCHES 已记录 |
| 本地测试缺口 | 已重新生成隔离项目；`cargo check --target aarch64-apple-ios-sim`、Xcode Debug build、受控 `ios.install` exit 73、未安装 app、lease owner 释放和 simulator 删除均有证据 |
| CI 缺口 | PR #381 已在 macOS desktop-template job 实际运行 iOS simulator Rust check，push/PR 两套 workflow 的 Linux/macOS/Windows、template、baseline-driver 均通过；该变更已合并为 `306c070` |
| GUI/设备缺口 | 本轮临时 simulator 已 shutdown/delete；本地责任变体已关闭。真实 iOS GUI/输入/语义及物理设备仍不在此出口 |
| acceptance / 依赖 | P-01 native-install-failure responsibility variant；F01 无硬前置，P01 仍要求 F01+T01；不宣称 P-01、F01 或 G0 完成 |
| 非目标 | 不在本轮扩展 accessibility、scene/present、移动语义/输入、真机或完整 iOS CI；不改上游 gpui API 语义 |
| 有界出口 | 已取得 install-failure + cleanup 证据；下一步只需新模板 CI/design-doc/review/merge，随后复核 F01/P-01 父任务剩余项并选择下一依赖可满足候选 |

### 下一执行卡：T01 doctor CLI host-smoke

| 维度 | 本轮登记 |
| --- | --- |
| 实现缺口 | 将 target-aware doctor 通过真实 CLI 调用接入 integration test：生成 desktop-only 项目，显式运行 `doctor --json --target desktop`，验证 schema v2、target source、required pass 和不引入 Android/iOS checks |
| 本地测试缺口 | macOS arm64 已通过 `cargo test --locked --test doctor_cli -- --nocapture`（2 passed），覆盖显式 target、项目默认 target 和非项目 host-only；需保留 stdout/stderr/exit 语义并执行适用 workspace 回归 |
| CI 缺口 | 已由 PR #382 两套 workflow 实际运行 Linux/macOS/Windows `check` matrix；run `37496876490`、`37496869916` 全部通过并 squash 为 `9e6ef64` |
| GUI/设备缺口 | 本切片不声称 GUI/device；真实 Linux/Windows host doctor、Android x86/unknown ABI 和 physical device 继续作为 T-01/T-03 后续责任 |
| acceptance / 依赖 | T-01 的 T-01 CLI target-selection/host-only responsibility slice；无硬前置；不晋升 T01 或 G0 |
| 非目标 | 不自动安装工具链，不修改 SDK/许可/签名，不把 CI hosted runner 当作完整 native device 验收 |
| 有界出口 | host-smoke 已收口；下一步只处理 T-02/T-03 的真实工具/设备责任变体，若平台或设备缺失则记录 required/unavailable 结果和恢复条件，继续下一个独立候选 |

领取任务后，将游标更新为任务 ID、本轮剩余项、执行阶段和下一动作；收口后移到下一任务。
设备、权限、review/merge 或 CI 等待必须另记恢复条件，不能让一个等待项卡住整个路线。
用户明确指定其他任务时优先执行用户任务，并在此记录范围变化；验收与真实性要求不变。
历史 22 个 `in_progress` 只表示有未完成实现/验收，不表示允许同时开展 22 条主线。
“不以扩张替代收口”是选题规则，不新增 `paused` 父任务状态，也不撤回已有可用实现。

### 2.1 首批候选示例：T01

下卡说明如何把一个候选变成可执行收口工作，不限制整体 prompt 的范围。若选 F01 或
其他依赖可满足的任务，按同样结构建立工作卡；不要为复用示例而强行选择 T01。

**先读代码**：[doctor CLI](../../src/commands/doctor.rs)、
[probe](../../src/toolchain/probe.rs)、[requirements](../../src/toolchain/requirements.rs)、
[report](../../src/toolchain/report.rs)；再读清单 T01 和矩阵 T-01/T-02/T-03。

当前可确认的实现：target-aware 报告、required/optional 规则、命令探测与超时边界已有。
`run_command` 在进程成功退出时返回 `CommandState::Passed`；进程执行成功不能独自证明
版本可解析、满足项目要求或 SDK/ABI 匹配。不要让所有成功的非版本探针都变成版本错误。

| 维度 | 剩余检查/工作 | 收口证据 |
| --- | --- | --- |
| 实现 | 区分进程成功、版本解析失败和版本不兼容；复核 Rust/JDK/Gradle/AGP/SDK/ABI 的 expected/actual 规则及秘密筛除 | 真实代码定位、明确适用工具/规则和未支持项 |
| 本地测试 | 先用 exit 0 + 畸形版本输出复现；覆盖非零、超时、大输出、合法/不兼容版本、required/optional 结果 | 修复前失败/修复后通过的针对性测试及 workspace 结果 |
| CI | 对本次提交执行已有三 OS 检查，复核模板/工具链 job 中与 T01 有关的结果 | 绑定提交的 job/artifact；本地通过不能代替 CI |
| 原生验收 | 执行 T-01/T-03 所要求的项目、SDK/JDK、ABI、多候选设备及秘密筛除变体；T-02 至少一个真实工具对照 | case/environment/commands、退出码、expected/actual、零配置修改证据 |

T01 的用例层级为 L0/L1，不额外要求 GPU 渲染；真实 SDK/设备选择证据仍不能由 shim
代替。没有环境时列出具体缺项和恢复条件，不把整个任务描述为“仅剩文档”。

**本轮有界出口**：完成版本校验切片的回归、实现和本地验证，明确 T-02 覆盖的变体；
补上其余 T01 缺口清单并更新本卡。这个出口是迭代完成，不是 T01 自动 `done`，也不是
整体推进的终点；继续处理该任务剩余项，或在满足切换条件后自动选择下一任务。

**父任务出口**：T-01/T-02/T-03 的 T01 责任变体、所需 L0/L1 证据和文档齐全，
提交审查；合并及验收确认后才能晋升 `done`。不因一次版本解析修复声称完整 doctor 已通过。

**非目标**：安装 SDK/接受许可、修改签名、扩大构建缓存、重写整个 runner 或改变协议。

**给下一 Agent 的第一步**：核对新提交后，用现有 probe 测试结构构造“退出 0、输出畸形
版本”的回归，核查 CLI/report 是否把它错误判为可用；然后确定最小版本校验接线。
若当前代码已修复，复用其证据并推进 T01 的下一个缺口，不重复实现。

### 2.2 本轮工作卡：T01 版本判定切片

基线：当前 `main`/`origin/main`=`1c0a4cb`，工作区干净；`852ddec` 到当前基线只有
路线文档变化。硬依赖：无。验收责任：T-02 的命令成功、畸形版本、有效版本分类；不宣称
T-01/T-02/T-03 或 G0 已整体通过。

| 维度 | 本轮开始时的剩余项 | 本轮出口/证据 |
| --- | --- | --- |
| 实现 | 已按 `expected.version` 解析命令版本；不带版本要求的命令仍只看可用性；project-selected Gradle wrapper 做精确比对，固定模板 AGP 9.1.0 + Gradle 9.4.1 建立 Java 17 最低规则，未知组合保留 Unknown。修正 iOS 单字符串 host、adb version expectation，并读取 SDK platform/build-tools、NDK metadata 和项目 Rust minimum；Android selected-device 现在比较 `Device.arch` 与配置的 `GPUI_ANDROID_ABIS`，缺失架构保持 Unknown | 本轮接入 Android ABI match/mismatch/unknown 判定；跨平台规则/CI、动态兼容和完整秘密筛除边界仍需核验，未建模 AGP/Gradle 组合继续保持 Unknown |
| 本地测试 | 修复前定向子进程回归确认为 exit 0 + 畸形版本误报 Pass；已有 T01 定向和 ABI 测试、串行 workspace 记录见 attempt-05/06。本次发现 Ubuntu 输出 `cc (Ubuntu 13.3.0-6ubuntu2~24.04) 13.3.0` 无 marker 后新增真实格式 shim：定向 parser test 1 passed、原 CI 失败的 upgrade no-op transaction test 1 passed；`cargo test --workspace --locked` 主二进制 489 passed/12 ignored，integration、协议与 xtask 测试均通过；clippy、build、fmt、diff check 通过 | 新增回归写入 `docs/experiments/T01-doctor-closeout-2026-10-06.md`；此前默认并行失败保存在 `artifacts/acceptance/1c0a4cb/T-01/macos-arm64/attempt-05/`，串行完整结果及日志在 `attempt-06/`；其它 T01/T02/T03 证据路径不变 |
| CI | PR #380 新 head `b2dbc40` 的两次 workflow run 均全绿：Linux/macOS/Windows、desktop-template、android-template、baseline-driver 通过；原 Ubuntu `cc --version` 失败现已在 Linux workspace job 中通过 | PR #380 已于 2026-10-06 以 squash 合并为 `f8192d6`；检查链接为 run `37436523210`、`37436528816`。T01 父任务仍因真实 x86/unknown ABI、physical device、Linux/Windows 原生 doctor 变体未收口 |
| 原生验收 | 已在 macOS arm64 对 desktop-only、iOS-only、Android-only 生成项目及非项目显式 target 跑 doctor；最终 desktop/iOS/Android 项目均返回 0，Android 实际核对 platform 34、可用 build-tools、NDK 27.2.12479018、Gradle 9.4.1、AGP 9.1.0、Java 21.0.8。iOS 22 台 simulator 中两个 selector 解析到不同设备且未启动；Android 发现 3 台 stopped AVD，两个 selector 解析到 Pixel_9_Pro / Pixel_9a；Pixel_9_Pro 在 `arm64-v8a` 构建 ABI 下通过，在 `x86_64` 构建 ABI 下 required check 失败，调用前后 AVD 均保持 stopped。`adb devices -l` 无连接设备；临时 SDK canary 未泄露；错误 compileSdk、缺 build-tools、无 NDK metadata、Java 11 均拒绝 | T-01/T-02 原始证据见 `artifacts/acceptance/1c0a4cb/T-01/macos-arm64/attempt-03/`、`T-02/macos-arm64/attempt-02/`；iOS selector、Android AVD selector、ABI target 缺失和 ABI match/mismatch 分别见 `T-03/macos-arm64/attempt-02/`、`attempt-04/`、`attempt-03/`、`attempt-06/`。仍需 CI、Linux/Windows、真实 x86/unknown ABI 或 physical Android device 验收，不宣称整个 T-01/T-03 通过 |

本机已补两个 iOS selector、两个 Android AVD selector、required Android ABI target 缺失
负例，以及真实 ARM64 AVD 的 build-ABI match/mismatch 结果；缺失架构的 Unknown 由 L0 单测
覆盖。本机没有 x86_64/未知架构设备，真实设备 mismatch 变体仍未运行。默认并行 workspace
的三个 process-tree 测试失败已保留，串行完整 workspace 通过；这不是 CI 通过。CI、Linux/Windows、
物理设备与 AGP/Gradle 兼容边界仍未收口，父任务保持 `in_progress`。F01 的本地 headless 切片及
未完成 GUI 变体见下节。

### 2.3 F01 headless 基线工作卡与交接

基线：领取时 `main`/`origin/main`=`1c0a4cb`，当前工作区含 T01 未提交实现和 F01 驱动修复；
F01 无硬依赖。责任子例：P-01 的 L0 supervisor span/失败样本、P-02 的固定 fixture 与
10+30 样本；不承担 T05/T06 索引/缓存联合对照，也不把 headless 结果当作 L2 UI 通过。

| 维度 | 本轮结果/剩余项 | 证据 |
| --- | --- | --- |
| 实现 | 基线脚本夹具补 `gpui-dev` feature；status predicate 对 `build`/`running` transitional null 安全；self-test 保持通过 | `scripts/live-baseline.py`、`cargo fmt --check`、`python3 -m py_compile scripts/live-baseline.py` |
| 本地测试 | macOS arm64 实跑 counter、login-invalid、list-scroll；每个 10 warmup + 30 measurement，共 120 样本、1515 spans；失败 30、恢复样本 30，span 无负时长；本轮完整 workspace 489 主二进制测试通过、12 ignored，integration/protocol/xtask 目标通过；clippy/build/fmt/design docs/diff check 通过。默认并行 workspace 的 3 个 process-tree timeout 单列在 T01 `attempt-05` | F01 数据见 `artifacts/acceptance/1c0a4cb/F01/macos-arm64/attempt-03/`；本轮完整 workspace 回归见 `T-01/attempt-07`；P-01 GUI/span 原始证据见 F01 `attempt-06/attempt-07`；失败尝试 `attempt-01`/`attempt-02` 保留原始原因 |
| CI | PR #381/382 push/PR 两套 workflow 的 Linux/macOS/Windows、desktop-template（含 iOS simulator target check）、android-template、baseline-driver jobs 均通过；workspace 包含 live failure/recovery/supersession integration test | PR #381 runs `37489285440`、`37489277701`；PR #382 runs `37496876490`、`37496869916`；P-01 当前 GUI/native 证据仍为本机责任变体，不把 hosted CI 当完整 F01 GUI/device 收口 |
| GUI/设备/native | attempt-05 注册 responsive macOS Counter 窗口并成功 `capture.window`；语义 `a11y_inactive`，截图来源/资源 current；受控 compile_error build b3 failed 后 build b4/run r2 responsive。attempt-06 在 responsive 窗口运行的 cargo.build b6 经本 session `q` 取消，父/子 span 均 `cancelled`。attempt-07 系统 `/tmp` generated project 中 build b4 被源码更新 supersede，后续 build b9 成功并恢复 responsive；窗口 PNG 51,105 bytes，scene/source/assets matches，但 `presented_frame_id=null`。stable retry semantics 返回 `runtime_unavailable`。修复模板 backtrace snapshot 后的 iOS simulator 重跑中，cargo target check、Xcode build 成功，受控 `simctl install` exit 73 进入 `ios.install`，app 未安装、lease owner 释放、临时 simulator 删除 | cancel/superseded/native-install-failure 已关闭 macOS 本地 responsibility variants；新模板 CI/review/merge、完整 F01/P-01 父任务、真实语义/输入/scene/present 和跨平台 GUI/device 仍未收口。完整 details 和 artifact 路径见 `docs/experiments/F01-p01-closeout-2026-10-06.md`；不能晋升 F01 `done` |

F01 本轮复核 attempt-04/05，并新增隔离 attempt-06/07 和修复后的独立 simulator 重跑：关闭 macOS
cancel/superseded/native-install-failure responsibility variants 并确认语义 provider 不可用；
仓库内原有 desktop/iOS/Android 长运行 live 进程未触碰；本轮启动的临时 desktop 和 simulator
session 均已停止并删除。

F01 与 T01 均无硬前置。PR #381 已 squash 合并并复核新基线；本轮已关闭 P-01 的三个 macOS
隔离 responsibility variants，但 F01/P-01 父任务仍缺 P-02 联合对照、真实语义/输入/scene/present
和跨平台 GUI/device 证据。T01 host-smoke 已由 PR #382 squash 合并并取得两套三平台 CI 通过；
T01 仍缺 T-02/T-03 的真实工具/设备 responsibility variants。P01 硬依赖 F01+T01，T02 硬依赖
F01；其它核心项依赖未满足，不绕过前置。下一动作是继续 T01 T-02/T-03 可执行切片，随后复核
T01 responsibility 出口并选择下一候选。

## 3. 自动选择、执行、收口循环

1. 核对当前分支/dirty diff/`origin/main` 与审计基线；阅读全部任务总表和执行游标。
   游标仍可执行则接续，否则按硬依赖和 G0→G4 顺序筛选；同批次优先收口已有实现且
   所需环境可用的任务。无硬前置的 F01/T01 是当前首批候选，不是永久指定。
2. 建立“实现 / 本地 / CI / 原生验收”四项差距，列明 acceptance case、variant、平台和
   证据位置。一个用例可有多个任务责任，必须写到子例，不能只写“单测全绿”。
3. 核对硬依赖和责任子例。已有代码可复用，但不能因下游已经存在就倒推上游完成。
4. 限定本轮改动与出口，先复现缺口，再补最小实现和回归；每次迭代应消除列出的具体剩余项。
5. 实现后的下一步优先为该任务剩余验收，不自动改成另一个能力切片。证据齐全进入审查；
   本地改动未合并不声称父任务完成，既有历史 `done` 也须复核其证据适用范围。
6. 当前任务收口后，更新证据/状态并自动返回第 1 步选择下一任务。已经完成本地实现/证据，
   但仅等待必要 review/merge/CI 时，记录等待而非 `done`，继续独立且依赖可满足的任务。
   不未经用户要求自行 commit/push/merge，也不以等待合并为由宣称硬依赖已完成。
7. 循环推进至授权范围完成、没有可执行任务而需要外部条件、用户要求停止，或本轮执行
   时间/上下文限制需要交接。不得因为一个 PR 切片或一个工作包结束就默认停止、请求
   “是否继续”；交接记录游标、等待项、候选队列和一个具体下一动作。

允许切换焦点的条件：任务已收口；仅剩必要 review/merge/CI 等待；有明确外部阻塞；用户
重新指定；发现可复现的正确性/安全缺陷需要优先修复。切换前必须记录原任务残余、原因、
替代任务的依赖和返回条件。等待任务恢复可执行后重新纳入候选，不能永久遗忘。
本地尚未尝试、任务难、需要更多测试、刚合并一个切片，都不是外部阻塞。

缺陷插队需写清复现、影响、最小修复、回归和返回点。修完返回原主线，不把一次 defect
扩张为“继续枚举所有 Gradle API”。无法证明任意脚本的输入闭包时保持既有 bypass/unsupported
边界；扩大支持范围另列范围和验收，不能成为默认无限任务。

外部阻塞时可继续处理同一任务不依赖该环境的剩余项；确实无法继续才选择下一个
依赖可满足的任务。如果没有这样的任务，报告缺失环境/需要的决策，不继续随机增加切片。

## 4. 收口候选队列

下表是选择顺序，不改变父任务硬依赖，也不是按行保证一轮完成。每行进入前要查
清单依赖及矩阵责任子例；同组也不能无视依赖启动。已存在代码只用于复用。

| 顺位/批次 | 收口范围 | 要消除的关键剩余项 |
| --- | --- | --- |
| 首批/基础 | F01/T01，按可执行性选择后依次收口 | 固定夹具、单调 span、真实基线和失败样本；doctor 版本/目标规则与原生证据 |
| 基线与可行性 | P01、T02，再 T03 | GPUI 观察能力与限制证据；可寻址历史模板内容、识别与升级计划 |
| 兼容与可信观察 | F02，再 O01/O02/O03 的责任复核、T04、O04 | 在线旧版本兼容/归档、runtime 发布边界、窗口/资源故障、升级恢复、真实 observe |
| 场景前置 | O05/O06、S01/S02、M03/M04 | 实际语义字段/稳定 ID、查询压力、静态校验、真实环境/reset、租约和冻结输入 |
| 桌面闭环与 Agent | S03、S04，再按依赖 A01/A02/A03 | 三夹具正常输入/断言/cleanup，连续 20 次及故障证据；相应 MCP/知识交付 |
| 跨端复验 | M01、M02、M05，Q01 收口 | 真实移动生命周期、完整三端同快照矩阵、复现和分层 CI |
| 优化与效果 | T05/T06、G01/G02、Q02 | 限定支持范围的索引/缓存/性能/Agent 对照，不追求无限 API 识别 |
| 可选支线 | S05/G03/M06/A04 | 不阻塞上述主线；用户指定或核心收口后再选 |

L2/L3 driver 和证据收集应随相关任务验收补齐，不等到 Q01 最后一次性建设。优化队列
靠后不等于搁置已发现的错误缓存命中等正确性缺陷，缺陷按插队规则处理。

### 4.1 依赖与历史完成状态复核

O03 的历史 `done` 保留，不在本次文档调整中自动撤回或扩大。其 F02 硬依赖尚未 `done`，
且 O-08/O-12 包含观察/真实图片子例：后续到兼容/观察批次时必须核对历史证据是否只
覆盖产物传输责任。若仅是责任拆分，分别登记，完整用例仍为未收口；若确有 O03 自身
证据缺失，列证据定位和缺项，再同步状态与统计。不以“不在 Git 中”推断证据不存在。

依赖变更或新增正式子任务必须有范围、验收责任、证据和受影响引用的同步修改。
不得以“使用到的子集能编译”为由跳过硬依赖。没有完成责任复核前，不批量修改
35 项状态，不新增虚假完成百分比；门槛通过仍需该门槛完整平台/用例证据。

## 5. 交接与进度更新

每次任务切换和会话/PR结束，更新第 2 节的游标、剩余项、等待项和下一候选；受影响的代码事实与父状态
同时写回 current-status/backlog。没有状态变化也需说明本次到底关闭了哪一个子例。
保持接续卡简短，不把本文变成逐提交流水账；历史命令和大产物放独立证据根。

```text
基线/当前提交与 dirty diff：
整体授权范围 / 当前父任务 / 本轮切片：
本轮已关闭项（case + variant + platform）：
实现剩余：
本地测试（命令/结果/证据位置）：
CI（commit/job/artifact；未运行则明确）：
原生验收（真实工具/GUI/device；未运行项及原因）：
硬依赖 / 共享用例其他责任未完成项：
父任务状态及是否已合并：
外部阻塞 / 切换理由 / 返回条件：
已收口任务 / 等待任务 / 下一批依赖可满足的候选：
下一 Agent 的一个具体动作：
```

本文与其他设计文档使用 `cargo x check-design-docs` 和 `git diff --check` 验证一致性。
这不能证明版本校验修复、真实工具链、GUI、设备或性能验收通过。

## 6. 可直接交给其他 Agent 的整体推进 Prompt

```text
请持续推进本仓库整体路线，而不是只完成某一个任务或实现切片。

先遵守 AGENTS.md，阅读 current-status、implementation-backlog、acceptance-matrix
和 closeout-plan，比较审计基线、当前分支、origin/main 及工作区改动，并以真实代码和
证据重新核对进度。不要覆盖已有改动，不要把历史审计、设计接口或状态标签当作现状。

目标：持续推进 G0–G4 核心工作包，优先收口已有实现，再实现依赖可满足的新任务。
自主选择任务，不绑定某个固定 ID，也不要机械延续最近提交的主题。每次保持一个实现
焦点，按硬依赖、门槛顺序、剩余缺口与可用环境选题；共享验收按责任子例登记，不能
跳过硬前置或降低完整用例/平台的验收标准。

对选定任务先列清实现、本地测试、CI、GUI/设备验收的剩余项，复现具体缺口，完成最小
实现与回归，运行 CONTRIBUTING.md 要求的适用验证并保存对应证据。完成切片后继续
处理该任务剩余项，不将局部成功冒充父任务完成；证据齐全进入审查，合并及验收确认
后才标 done。不得为了增加完成数修改标准，不未经明确要求自行 commit/push/merge。

任务收口后立即更新路线状态、执行游标及证据，自动选择下一项继续。若只剩外部设备、
权限、CI、review/merge 等等待，记录具体原因和恢复条件，继续独立且依赖可满足的
任务，不让一个等待项阻塞全部路线。正确性/安全缺陷可有界插队，修复后返回主线；
不要无限扩张缓存 gate、证据字段或可选功能来替代任务收口。

不要在完成一个切片或任务后默认停止，也不需要逐项询问是否继续。持续执行到授权
范围完成、所有可执行路径均需外部条件、用户要求停止，或本轮执行限制需要交接。
结束时列出本轮推进/收口的任务、未完成原因、分层验证结果、等待条件和下一动作，
并更新接续记录，使下一 Agent 能沿整体路线继续，而不是重新猜测选题。
```
