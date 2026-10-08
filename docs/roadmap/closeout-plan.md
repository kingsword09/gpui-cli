# 整体路线推进、收口与 Agent 接续

更新日期：2026-10-08（Asia/Shanghai）。当前 `main`/`origin/main`=`ea9f5d8`（PR #392 squash）；
前一基线 `8effdf9` 是 PR #388 的分层移动 CI 合并，`f38d9df` 是 PR #389 的合并与审计基线。
PR #392 新增 F01/P-01 clock-offset L0 regression，不改变 span runtime 行为。PR #388 相对
`f38d9df` 的 13 个文件变化覆盖 CI、driver、文档和贡献说明；F01 GPUI scene runtime 未变。
PR #388 push run
[`37729846375`](https://github.com/kingsword09/gpui-cli/actions/runs/37729846375) 与 PR run
[`37729843264`](https://github.com/kingsword09/gpui-cli/actions/runs/37729843264) 各 14 jobs 全绿。
新 Android x86_64 AVD doctor artifact 已核验真实 ABI match pass、配置 mismatch fail；smoke
仍为 `verified_present=false`、`gui_acceptance=not_run`。T01/F01/P01 父状态没有晋升。
更早的 T01 doctor、F01 baseline-driver、iOS-safe backtrace、PR #382–#389 的合并证据保留在下文历史卡。

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
| 当前执行游标 | F01 baseline-driver CI artifact：当前分支已接入完整 10+30 baseline、source manifest、样本/span verifier 和 always-upload artifact；PR #392 的 P-01 skew L0 已 squash 合并，本切片待 PR CI 与干净 hosted artifact 核验 |
| 首批候选 | F01 无硬前置；其 P-02 责任只包括固定夹具和原实现基线。T05 负责索引对照、T06 负责缓存对照，完整 P-02 不作为 F01 的反向依赖。T01 x86_64 emulator match/mismatch 已由 PR #388 hosted artifact 覆盖；unknown-ABI physical device 仍未覆盖 |
| 选题原则 | 可恢复的游标优先；否则按第 3 节规则选择，不能将示例任务当作永久主线 |
| 正式状态 | 仍为 1 done、22 in_progress、12 planned；选择或切换游标不等于晋升 |
| 不作为默认替代 | 用 T05/T06 优化、预热、API marker 或移动证据字段扩张绕开早期任务；它们依赖/范围满足后仍可按队列选择 |

### 当前工作卡：F01 baseline-driver CI artifact

| 维度 | 本轮开始时的差距 / 出口 |
| --- | --- |
| 实现 | 当前分支将 `baseline-driver` 扩为构建 CLI、记录 revision/dirty 与 workflow/driver/verifier/CLI hashes，运行 3 个固定 fixture 各 10 warmup + 30 measurements，逐样本校验结果、恢复、span 和命令，并在 job 失败时仍上传 report 与 CLI binary（14 天） |
| 本地测试 | macOS arm64 dirty smoke 已完整运行并通过新 verifier：120 samples、30 warmups、90 measurements、40 compile-failure samples、40 successful recoveries、1520 spans、993 commands；CLI SHA-256=`8c9a8cd20e0ac38f9fc2741bdb52c7db1a86e4bcfd5508f2c14a5eeec8fc2e86`，原始目录 `artifacts/acceptance/ea9f5d8/F01/macos-arm64/attempt-12/`。`cargo build --locked --bin gpui`、Python compile、driver self-test、actionlint、design-doc 和 diff checks 通过。既有大输入 release oracle 为 4096×8192 bytes、10 warmup + 30 paired，P50/P95=167.20/171.18 ms，correctness 计数为 0，raw JSON 在 `artifacts/acceptance/8effdf9/F01/macos-arm64/attempt-11/large-input-release-benchmark.json` |
| CI | 本切片 PR 尚待运行；必须核验 PR/push 两套 workflow 的 baseline job、下载实际 artifact 并检查 source revision、`dirty=false`、三个 fixture 的 10/30 数量、失败/恢复、span/commands 与 CLI hash。PR #392 的 P-01 skew CI 已由 run `37755266052` 和 push attempt 2 通过；attempt 1 的 host-only timeout 保留为历史失败 |
| GUI / 设备 | P-01 的 macOS cancel/superseded 与 iOS simulator native-install-failure 已有本机证据。scene readback、verified present 和完整语义属于 P01/O-10/O-11 observer work；PR #388 mobile process/capture smoke 未通过这些验收 |
| acceptance / 依赖 | F01 无硬前置；P-01 需要 clock-skew L0 与既有 L2 故障样本。共享 P-02 中 F01 负责原实现基线，T05/T06 各自负责 index/cache 比较；完整 P-02 仍未通过 |
| 非目标 | 不在 F01 中伪造 GPUI scene/present/accessibility provider，也不把 oracle/index 局部对照写成 T05/T06 或完整 P-02 完成 |
| 有界出口 | 为本切片开单一 PR，核验 PR/push jobs 和实际 artifact 的 source/dirty/样本数/summary/hash；随后重审 F01 task-local P-01/P-02 责任并自动选择下一项。CI job 通过但 artifact 缺失时不能把 F01 改成 `done` |

### 2026-10-08 用户指定切片：分层移动 CI 初始工作卡

以下工作卡保留各轮领取时的缺口和切换理由；当前出口以本节上方游标及
[移动 CI 切片的最终结果](../experiments/mobile-ci-layers-2026-10-08.md)为准。
实现证据基线为 `5d09ab3`，后续文档同步不改变该基线。已重新核对 `origin/main`=`2f85842`；
合并/主线 CI、完整 GUI/真机仍是独立等待项，本轮没有授权范围内的剩余实现任务。

| 维度 | 本轮登记 |
| --- | --- |
| 基线 / 范围 | 初始 `HEAD` 与 `origin/main` 均为 `2f85842`；已复核 `f38d9df` 之后三份路线文档变更。现有 PR #388 分支以 `49a6a3a` 为起点合入该主线并解决冲突，用户已授权提交推送。本轮只落实用户指定的 Android/iOS 分层 CI，不启动整体路线循环 |
| 实现缺口 | 已实现 Android/iOS cold doctor、Android live doctor 的 KVM preflight/boot deadline/失败报告、独立完整 Debug native build 与进程/capture/cleanup smoke；cold/live 均准备 required cargo-ndk，初始生成 Cargo.lock，合法 Gradle cache bypass 不阻断真实 APK 检查 |
| 本地测试缺口 | 23 个 driver 回归通过，覆盖 failure-before-assert、ABI/phase/identity、PNG 校验/解压、timeout/秘密筛除、独立 build、manifest 目录根/cache bypass 与 failed-install cleanup；iOS/Android ARM64 cold doctor、完整 iOS native build 和 ARM64 APK 已通过。本地 iOS boot-first smoke 300s 等待 System App 超时但 owned simulator 删除通过；新 build-first runtime 仍待实际运行 |
| CI 缺口 | `d9dc35b` 的 push/PR runs `37714516656`/`37714519942` overall=failure；仅 iOS smoke 的 boot 后工具 timeout 失败，其他 jobs success。push cold/live doctor reports/Android PNG 与 summary 已严格复核。新的 timeout-only 重探已获用户授权提交推送，须绑定新 workflow run/job/artifact，不继承旧主线全绿结果 |
| GUI / 设备缺口 | process/capture smoke 不证明 scene readback、verified present、输入/语义、真实前台归属或 physical device；Metal probe 失败须留下 failure，不跳过后声称运行通过 |
| acceptance / 依赖 | T01 的 T-01/T-03 cold inventory 与 live x86 ABI responsibility；T01 无硬前置。完整应用 smoke 是 F01/P01 的平台可行性证据设施，不晋升依赖 F01/T01 的 P01 或 M01/M-01/M-02 |
| 非目标 | 不增加 XCTest/instrumentation 测试系统、不修改 GPUI renderer、不接公共 PR self-hosted runner、不降低原 live ABI 责任、不宣称 GUI/真机验收或父任务完成 |
| 有界出口 | 本地实现、23 个 driver 回归、两端 cold doctor/独立 Debug native build 和失败/cleanup 证据登记已完成；PR #388 本地整合及文档冲突已解决，用户已授权提交推送，等待 review/merge/新 CI。发布后核对 raw doctor、native build、boot/runtime/capture/cleanup artifacts。x86 live、真实首帧/scene/输入/语义和 physical variants 不以本地 smoke 替代；不改 35 项计数 |

PR #388 的历史取消保留，但不再据此认定 hosted Linux 不支持 emulator：日志中
KVM 权限不足导致 `-accel off`，须先修复可配置权限并执行 acceleration preflight。
分层实现与本地 PR 整合已完成，提交推送已获授权，下一动作是核对 hosted 各层证据；原 F01
scene/present 游标暂因用户明确指定该切片而让位，退出后恢复其未完成项。

同日首轮 CI 失败复核的有界出口：仅补齐已证实的宿主库、cold AVD metadata 根和
iOS required tools，不修改 doctor ABI/required 策略或 renderer。27 项回归、actionlint、
Python compile、文档/diff checks 和本地 ARM64 isolated cold match/mismatch 已通过；
修复已获提交推送授权，须核对重跑的 x86 metadata/live 以及 iOS build/boot/runtime/cleanup。
失败报告、当前代码、父任务状态和 GUI 缺口保持分离；不从 Android APK pass 推导 GUI pass。

第二轮有界出口：`d9dc35b` bootstrap 各层已取得 hosted job/部分 raw evidence，唯独
iOS smoke 在 build/boot 成功后多工具超时。每个 live iOS doctor 用例只允许两次、
间隔 15 秒的明确 timeout-only 重探，逐次报告保留；缺工具/版本/target/selector 错误
不重试，最终不通过仍失败。33 项回归、Python compile、actionlint、文档/diff 检查通过；
本轮修复已获用户授权提交推送，尚未取得新 runtime pass。下一动作是新 CI 核验最终报告及真实 smoke，
不是晋升 GUI/首帧/父任务。

### 第三轮 CI 排障工作卡（`82a353c`）

接续用户指定的 PR #388 CI 修复；已 fetch 并比较主线 `2f85842` 与审计基线
`f38d9df`，期间仅路线文档同步。当前 head=`82a353c`；push run `37719062475`
全绿，PR run `37719065008` 仅 iOS smoke 失败，其余 13 jobs 通过。

| 维度 | 剩余缺口 / 本轮出口 |
| --- | --- |
| 实现 | PR 日志含 `ios.rust_target.simulator` 的 `Rust target probe timed out`，现有 timeout-only 白名单未覆盖该明确超时原因，首轮即失败；补齐该分类，保留缺工具/缺 target/版本/selector 等非超时拒绝 |
| 本地测试 | 新增回归先复现零次重试；修复后 36 项通过，覆盖 Rust target 正负 selector 恢复、持续超时上限及非超时拒绝；Python compile、actionlint 和文档/diff checks 通过 |
| CI | 失败/成功选定原始证据已下载核验：PR 确为零重试，push 重试一次后完整 doctor/Metal/install/PID=36244/1179×2556 PNG/hash/cleanup 通过；沿用提交推送授权，修复后须核对新 push/PR runs，不从旧 push 全绿推导 PR 通过 |
| GUI / 设备 | 保留 `verified_present=false`、`gui_acceptance=not_run`；真实首帧、输入/语义、physical device 仍无验收 |
| acceptance / 依赖 | T01 的 T-01/T-03 iOS live selector 与 required-only failure 责任，无硬前置；smoke 不关闭 F01/P01/M01 父任务或依赖 |
| 非目标 | 不放宽 5s/30s CLI 预算、不扩大重试次数、不改 renderer、Android 或 required 策略；历史 probe 实测超预算不能称为硬截止通过 |
| 有界出口 / 恢复 | 漏分类修复及有意义回归通过，提交推送，核验新 CI 和原始证据并同步状态；review/merge 与 GUI/真机责任继续独立登记。整体路线恢复后才返回 F01 scene/present/semantics |

### 第四轮 CI 排障工作卡（`a43b3c8`）

Rust target 超时漏分类已修复并推送。push run `37721318579` 的 14 jobs 全绿；
PR run `37721322431` 的 13 jobs 通过，iOS smoke 转为 `simctl launch` 180s 超时。
两套 live doctor 均在一次重试后通过；PR Metal/install 通过，launch 输出为空、owned
UDID 清理通过。push 的 PID=26376、1179×2556 PNG/hash 与 cleanup 已核验。

| 维度 | 剩余缺口 / 本轮出口 |
| --- | --- |
| 实现 | 原始两分钟 diagnostics 为 683057564 bytes / 474956 events，其中 apsd 405664 events，反复 simulator certificate unsupported / reconnect；没有应用 bundle/executable 记录。仅对 CI 自建 simulator 的 smoke 显式关闭 APNs 后台服务并记录验证结果，再实测是否消除启动失败；不声称日志已证明唯一因果，也不重试/吞掉应用启动失败 |
| 本地测试 | 新建本地 iOS 26.2 simulator 的最终 driver 实跑通过：实际 user domain、disabled、bootout、service-not-found 与 owned UDID 删除均有原始证据；40 项 driver 回归和 Python compile/actionlint/design-doc/diff checks 通过。此本地服务探针不覆盖 GPUI 应用 launch |
| CI | 新失败已绑定 run/job/artifact；修复后重新核验 push/PR live doctor、launch、稳定 PID、PNG/hash、cleanup，不将 push 单边成功算作整体验证 |
| GUI / 设备 | 仍不声明 app-owned pixels、verified present、语义/输入或物理设备验收 |
| acceptance / 依赖 | 同一用户指定移动 CI 切片，T01 无硬前置；启动 smoke 为 F01/P01 平台证据设施，不晋升父任务或解除硬依赖 |
| 非目标 | 不扩大 timeout、不重试 launch、不吞非零退出/真实 crash；不修改宿主或既有 simulator 服务，不扩展 renderer、Android 或核心路线。此 smoke 不覆盖 APNs 推送能力 |
| 有界出口 / 恢复 | 收集启动失败原始证据，完成针对性修复和回归，发布并核验两套 CI；如有外部环境阻塞，记录准确恢复条件，不伪造 runtime pass。review/merge 与完整 GUI/真机责任独立保留 |

### 第五轮环境准备纠偏（`f78aee2`）

push run `37724354473` 的 13 jobs 通过，iOS smoke 卡在新增的 `launchctl disable`
60s deadline，尚未进入新 doctor/launch；PR run `37724359228` 的 14 jobs 全绿。
持久化 disabled override 对本次只 boot 一次、结束即删除的 owned simulator 并非必要。
本轮把 APNs 隔离收敛为 `bootout` 当前 service registration 加确切 absence 验证，
不再写/读取持久化 disable 状态；报告只声明当前 boot 已卸载，不声明 persistent disable。
保持原服务控制/应用 launch 的 deadline 与单次 launch，不重跑或吞掉应用失败。
先补“不允许 bootout 失败仍通过”及 absence/域校验回归，再在新的本地 iOS 26.2
simulator 验证直接 bootout 和 cleanup；后续须绑定新 push/PR 原始 runtime 证据。
父任务、GUI/真机与 APNs 验收边界保持不变，失败实验不标作已稳定修复。
PR 原始证据已核验：service removal 后 doctor 无重试，launch 1.49s，PID=20494、
1179×2556 PNG/hash 与 cleanup 通过。精简后的直接 bootout driver 又在新建本地
iOS 26.2 simulator 验证当前 boot 服务移除及 cleanup 通过；41 项回归、Python compile、
actionlint、design-doc/diff checks 通过。下一动作是发布并核验精简版两套 hosted CI。

### 第六轮独立 workspace 测试竞态（`2cd4f00`）

push run `37726135008` 的 macOS workspace job 在
`current_app_channel_queues_accepted_asset_reconciliation` 失败，len=0、期望 1；
矩阵 fail-fast 取消 Linux/Windows，不能算三个独立测试失败。移动 jobs 仍在运行。
日志留存于 `artifacts/ci/37726135008/macos-check.log`。这是本次 CI 范围内独立可修的
测试同步缺陷；移动 runtime 暂等待 hosted 结果，未更换父任务或扩大整体路线。

| 维度 | 本轮出口 |
| --- | --- |
| 实现 / 原因 | app channel 先 emit journal event，再 push reconciliation queue；测试只等 event 后立即 drain queue，不能保证队列已发布。修复仅让测试等待实际 queue，保持事件和 payload 断言及原 5s wait budget |
| 本地测试 | 临时 test-only 在 event/queue 间注入延迟，验证旧测试稳定失败、修复后通过；随后撤销注入，执行 fmt/clippy/完整 workspace test/build 与文档检查 |
| CI | 先保留 `2cd4f00` 移动结果，避免取消还在运行的验证；发布测试修复后核验新的三平台与移动 jobs，不用单项重跑掩盖失败 |
| 验收 / 依赖 | O02 的 O-07 L0 queue responsibility，仅测试同步；O02 仍依赖 F02。不是 O-06/O-07 的真实资源/GUI 验收，不晋升任何父任务 |
| 非目标 / 恢复 | 不调整 app channel 的事件/队列语义、不扩大 timeout、不增加 runtime test hooks；此缺陷与 APNs 环境问题分别留证。测试出口后回看同一 PR 的移动等待结果，最终停在 review/merge 等待态 |

### 第七轮服务准备预算与最小调用（`2cd4f00`）

同一 push run 的 iOS smoke 在新增 service-before 只读查询上超过 60s，尚未
卸载 APNs。该 60s 是前轮新加的 driver 准备预算，不属于 T-02 的 CLI 5s/30s 或
应用单次 launch/180s 验收要求；此前 persistent disable 已有超时记录，不能继续
把 60s 当作 hosted 原生命令的充分预算。
本轮省去非必要的前置查询，直接通过 owned UDID 的 `user/foreground/com.apple.apsd`
bootout，再从确切 service-not-found / uid 报告核验移除和实际域。两条准备命令采用
现有原生命令的 180s budget，明确记录配置值，继续保留实际耗时/失败；不重试 launch，
不放宽 doctor 或应用验收，不声明硬 wall-clock 上界。新增 parser/错误回归引用已取得的
真实 stderr 形状；最终仍要求新 hosted runtime 通过。
reconciliation 测试已在受控延迟下通过且注入已撤销；首次完整本地测试另遇三个历史
process-helper 时序失败，分别复验并保留原失败，不把它们混成此次队列测试失败。
先完成测试/准备命令这两个已证实 CI 缺口的本地出口，再发布、核验所有 jobs 与原始证据。
本地出口已完成：41 项 Python 回归、fmt/clippy、默认并行完整 workspace 复验
（主二进制 489 passed / 12 ignored，全部其他目标通过）、build 和文档/diff checks
通过。三个原 process-helper 失败均保留原日志，定向与完整复验分别通过；production
app channel 未变，临时注入已撤销。下一动作是本次提交的两套 CI 和原始 runtime 核验。

### 历史执行卡：F01 / P-01 iOS simulator native-install-failure

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
| 本地测试缺口 | macOS arm64 已通过 `cargo test --locked --test doctor_cli -- --nocapture`（4 passed），覆盖显式 target、项目默认 target、非项目 host-only、required `cc` nonzero 和 `rustc` exit-0 malformed version；fmt/clippy/diff check 通过 |
| CI 缺口 | PR #382 两套 workflow 实际运行 Linux/macOS/Windows `check` matrix；run `37496876490`、`37496869916` 全部通过并 squash 为 `9e6ef64`。PR #383 两套 workflow 继续运行 target-selection 扩展；run `37500590204`、`37500498141` 全部通过并 squash 为 `757f05b`。PR #384/#385 的两套 push/PR workflow 也全部通过；runs `37506602963`/`37506608934`、`37509603211`/`37509630396`，分别 squash 为 `285893f`/`287c3c7` |
| GUI/设备缺口 | 本切片不声称 GUI/device；真实 Linux/Windows host doctor、Android x86/unknown ABI 和 physical device 继续作为 T-01/T-03 后续责任 |
| acceptance / 依赖 | T-01 的 T-01 CLI target-selection/host-only responsibility slice；无硬前置；不晋升 T01 或 G0 |
| 非目标 | 不自动安装工具链，不修改 SDK/许可/签名，不把 CI hosted runner 当作完整 native device 验收 |
| 有界出口 | host-smoke、required nonzero/malformed-successful-version 与 GitHub Linux/macOS/Windows host evidence 已收口；PR #388 x86_64 emulator 两次 hosted boot 未完成，记录 `emulator_boot_unavailable`，未取得 match/mismatch JSON。T-03 的 x86/unknown ABI、physical device 和 AGP/Gradle 边界仍需对应环境；环境缺失时保持 required/unavailable 记录，不晋升 T01 |

### 等待整体路线恢复：F01 GUI/scene/present 与 P-02 联合对照

| 维度 | 本轮登记 |
| --- | --- |
| 已有证据 | F01 macOS arm64 已完成三夹具 10 warmup + 30 measurement、failure/recovery、cancel/superseded/native-install-failure responsibility；原始 attempt-05/06/07 和 `F01-p01-closeout-2026-10-06.md` 已记录窗口截图、span 与 cleanup。主线 `7336d74` 的 attempt-10 又复跑真实 `capture.window` metadata（scene epoch/source/assets freshness）和 semantics `a11y_inactive`，没有新增可下载 PNG artifact |
| 剩余实现 | 先复核 scene readback、presented frame、语义导出和 P-02 联合对照的实际代码/证据边界；不把缺失 provider 的 `a11y_inactive` 或 `presented_frame_id=null` 伪造为通过 |
| 本地测试 | 已在独立 generated Counter 中重跑 live build/run、window observe 和 semantics observe；证据保存 command/status/operation JSON、artifact hash 与能力错误。若能力不可用，记录最小上游/恢复条件；下一步只补 scene/present 或 P-02 responsibility，不重复制造同一 `a11y_inactive` 结果 |
| CI/GUI 责任 | 现有 workflow/template CI 只证明构建和测试路径，不替代真实 GUI/scene/semantic/device；跨平台 GUI/device 与 T05/T06 联合对照仍未闭合 |
| 有界出口 | 关闭一个可复现的 scene/present/semantic responsibility variant，或记录明确外部阻塞与恢复条件；随后回看 F01 父任务，不晋升 F01/P01 |

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

历史工作卡基线：`main`/`origin/main`=`1c0a4cb`，工作区干净；`852ddec` 到当前基线只有
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

历史工作卡基线：领取时 `main`/`origin/main`=`1c0a4cb`，当前工作区含 T01 未提交实现和 F01 驱动修复；
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
