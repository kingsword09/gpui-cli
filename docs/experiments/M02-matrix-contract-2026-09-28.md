# M02：local matrix contract and scheduler（2026-09-28）

状态：in_progress。本切片先固定本地 matrix 的 cell lifecycle、deadline、fail-fast、
required/optional 汇总和 artifact 保留接口；当前已加入配置展开、分发前 admission、
并行 executor、mobile lifecycle adapter、matrix CLI 和移动 scenario driver 的 control/native
capture 接入。完整 simulator/emulator 运行证据仍未宣称。

## 已交付

- `MatrixPlan` 校验非空 plan/cell、唯一 cell id、max_parallel 上限和 global/per-cell timeout。
- `MatrixScheduler` 按 plan 顺序确定性 dispatch，区分 queued、running、cancel_requested 和
  terminal 状态；active cell 的取消必须先经过 cleanup，再以 cancelled 完成。
- global/per-cell deadline 会请求取消，不会在 cleanup 尚未结束时伪造 terminal success；
  fail_fast 取消未开始 cell 并请求 active cell cleanup。
- required cell 的 failed/cancelled/inconclusive/unavailable 都不能汇总为 passed；required
  全部通过但 optional cell 缺失或失败时汇总为 partial，并保留 cell artifact ids。
- `MatrixCellRunner` execution boundary 已接入：runner error 变成 failed cell，cleanup error
  覆盖原本的 passed，cell deadline 传入 adapter；初版顺序 harness 保留为测试/兼容入口，
  当前 CLI 已使用下述 parallel executor。
- executor 会在 cleanup 后再次检查 deadline；即使 adapter 迟到地返回 passed，cell 仍记为
  cancelled，已有 artifact id 保留供诊断。Windows CI 曾暴露一项既有 heartbeat/semantics
  socket 测试的帧顺序假设，测试现会有界地应答 heartbeat 并继续等目标 semantics query。
- `matrix.toml` admission 会校验 frozen source、target/scenario 引用、required/timeout、
  platform-specific device/ABI 约束，并展开稳定的 target×scenario cell。
- admission 会把项目目标、runner/宿主平台、设备 ABI、场景 capture requirement 和有界
  toolchain probe 汇总为 cell 级 unavailable 原因；Windows 等不可运行目标不会被省略。
- scheduler 接受 admission 产生的 unavailable cell，不启动 runner、不执行 cleanup，但在
  report 中保留 required/optional、错误码和后续聚合语义。
- cell 可携带有界、规范化的 resource ids；matrix resource pool 在同一 supervisor 内让
  共享设备资源串行、独立目标并行，并把等待纳入 cell deadline。
- 新的 parallel executor 为每个 cell 创建独立 runner，按 max_parallel 收集结果，保留
  runner error、迟到成功、cleanup failure 和 artifact ownership 语义；旧的顺序 adapter
  继续作为测试/兼容入口。
- mobile matrix adapter 已把现有 iOS simulator/Android runner 接到统一 cell 生命周期：
  prepare 前建立可 fencing 的 run identity，随后执行 launch、设备 PNG capture 和 native
  logs；process identity/log assignment 不确定时为 inconclusive，capture artifact 仍保留。
- mobile cleanup 即使 prepare/launch/capture 中途失败也会尝试 stop_owned，随后释放本次
  host lease；它不关闭用户启动的 simulator/emulator，也不把 lease 丢失当作成功。
- scenario executor 在初始 prepare/reset/observation 阶段失败时，也会先调用 runner 的
  finalization boundary 再 cleanup；因此已启动但未发布 `scenario_ready` 的移动 preview
  仍有机会收集 native logs 和其他 post-run evidence，原始失败仍保留为主错误。
- 如果移动 preview 在 `scenario_ready` 之前的 registration/launch 等待阶段失败，matrix
  driver 也会先尝试收集 native logs、owned stop 和 lease release，并把 `mobile_evidence`
  与已有 artifact ids 返回到失败 cell；这条路径不生成伪造的 `CheckReport`。实际 preview
  run 尚未发布时，证据明确写入 `run_id_bound: false`，日志保持 unassigned，不把准备阶段的
  run ID 冒充为实际运行身份。
- mobile cleanup 会独立记录 `stop_owned` 与 lease release 的错误到
  `mobile_evidence.cleanup_errors`，即使 stop 失败也继续释放 lease；因此 cleanup failure 不会
  丢失 run/device 归属证据，也不会把未执行的 stop 伪造成成功。
- matrix executor 在串行和并行路径中都会在 cleanup 完成后重新读取 runner context；mobile
  adapter 因而能把 launch、capture、native logs、stop、lease release 和 cleanup errors 等
  lifecycle evidence 保留到 `MatrixReport.context.mobile_evidence`。capture-only cell 仍不生成
  没有 steps 的伪造 scenario `CheckReport`，但其可取得的生命周期证据不会因 cleanup 后才完成
  而丢失。
- capture-only matrix adapter 和 control-driven scenario runner 都会把 `RunnerInfo` 与
  `RunnerCapabilities` 写入 `mobile_evidence`，使报告能够追踪选定 runner、host、平台、架构、
  设备类型、工具声明和能力边界；这些是声明性绑定，不冒充实时设备/前台/viewport 探针。
- 同一 evidence 还会保留 adapter 已收集的有界 `EvidenceLog`，覆盖 install/launch/capture、
  process/channel、native logs 和 stop 事件；每个 run 最多保留 128 条事件，单条 `details` 的
  序列化结果最多 16 KiB。超过事件上限时淘汰最旧事件并保留最近生命周期记录，超过 detail 上限
  时写入摘要/hash；`truncated` 与 `dropped_events` 说明是否发生边界收缩，JSON 反序列化也会
  重新执行相同边界。事件序列用于诊断归属，不把纯 Rust 测试当作真实 simulator/emulator
  fault matrix。
- capture-only matrix context 还会保留每个 `CaptureArtifact` 的 provider、相对路径、字节数、
  hash、像素尺寸、逻辑 viewport、scale、方向、系统 UI 标记和保守的前台包名；缺失的 probe
  字段继续保持 `null/unknown`，不会从 PNG 尺寸反推设备环境。
- action operation 返回 failed、unknown、cancelled、unavailable 或 timeout，且错误详情带有
  `operation.operation_id` 时，scenario step 会保留对应 action status 和 operation ID；这只是
  失败/不确定结果的归属证据，不改变 step/check 结果。unknown 仍为 inconclusive，不得重放动作
  或据此伪造 passed。
- gpui check --matrix <file> 已接入 admission、并行 executor 和本机 desktop scenario
  runner；它输出完整 matrix report，required 非 passed 会以失败退出。移动 cell 现在通过
  control-driven scenario runner 执行同一套 step/observation/action/reset 边界，并由父进程
  持有 host lease，子 preview 复用该 lease 保护的 control session。
- mobile scenario 的 screenshot/`capture.device` requirement 会保留 control observation，
  再通过 iOS simulator 或 Android runner 的 native PNG capture 补回
  `ScreenshotEvidence`；设备截图仍明确标记为 device scope、不可与 scene baseline 比较，
  不会被当作 `capture.scene`。
- 移动 admission 只将 screenshot、`capture.device`、semantics、bounds、reset 和 pointer/
  keyboard 能力纳入本地 driver 的候选范围；`capture.scene`、`capture.window` 和 runtime
  未声明的完整语义/输入能力仍按 capability 返回 unavailable/inconclusive。

## 2026-09-29 mobile scenario driver 边界

`gpui check --matrix` 的移动 cell 现在会将 target 的显式 device/ABI 传给 `gpui preview`，
通过 devserver control registration 等待 `scenario_ready`，执行 scenario executor 的
observation、action、wait、reset 和 assertion。scenario runner 仍是 control-driven：
原生 runner 负责 device lease fencing 与 PNG capture，control runtime 负责 GPUI 窗口内的
语义和动作路由。

本切片的证据边界如下：

- 本地有可用 simulator/emulator、toolchain 和 runtime capability 时，driver 可以执行真实
  的移动 preview 路径；本仓库当前 CI 和纯 Rust 测试没有这样的设备运行证据。
- native device screenshot 包含系统 UI，provider/dimensions/artifact id 会进入 check
  observation，但 `capture.scene` 仍在 admission 阶段 unavailable，不能用设备截图冒充
  scene capture 或视觉 baseline 通过。
- semantics、input 和 reset 必须由当前移动 runtime/control provider 实际声明并成功响应；
  admission 或 screenshot 成功不会替它们伪造通过。
- early prepare failure 的 finalization 只保证证据收集顺序，不把未 ready 的 preview 变成
  passed；finalization 本身失败仍按 inconclusive/cleanup 边界记录。
- pre-`scenario_ready` 的 launch/registration failure 现在同样保留可取得的 native-log/stop/
  lease evidence，但没有 scenario steps，因此只记录为失败 cell，不生成完整 scenario report；
  未绑定实际 run 的日志保持 unassigned。
- cleanup failure 现在同时保留在 scenario cleanup error 和 mobile evidence 的有界错误列表中；
  stop 与 lease release 分别尝试，lease release 不因 stop failure 被跳过。
- cleanup 完成后，matrix report 仍会读取 runner context；因此 capture-only mobile lifecycle
  cell 可以展示 launch/capture/native-log/stop/release/cleanup evidence，同时继续保持没有
  scenario steps 就不生成 scenario `CheckReport` 的边界。
- mobile evidence 还会保留 runner identity/capability metadata；缺失或不可信的实时设备状态
  仍必须由平台 probe/真实运行验收报告，不能从 `RunnerInfo` 推断。
- `EvidenceLog` 的 boundedness 只限制事件数量和 detail 序列化大小，保留最近事件并报告收缩诊断；
  它不自动增加 crash、ANR、重连、前台确认或完整 stop/log 语义，仍需真实平台运行记录。
- capture environment 字段的传播只补齐报告可见性，不把 capture-only cell 提升为完整 scenario
  steps，也不把保守 foreground marker 变成真实前台切换验收。
- event log 只传播已有 adapter 事件，不自动增加 crash、ANR、重连或前台确认；这些仍需真实
  平台运行记录。
- 已投递 action 的错误若附带 operation ID，报告会保留该 ID 以便把结果关联到原 operation；
  unknown 结果仍为 inconclusive，不能重放或降格成普通失败/通过。
- 没有完成 frozen snapshot build、真实 macOS+iOS simulator+Android emulator 矩阵报告、
  远程 runner 或完整设备故障矩阵；这些仍是 M02 后续验收。

## 2026-09-29 Android 真实 capture 探针

- 并行 cell 为每个 preview 注入唯一 session key，并按 registration target suffix 定向发现
  control session；同一项目的多个 desktop/mobile preview 不再因 `ambiguous_session` 等到
  cell deadline。
- Android preview 将 scenario 元数据和 fixture 通过 app-private `gpui_live.txt`/
  `gpui_preview_fixture.json` 传入；生成 runtime 在 Android 环境初始化 preview registry、
  fixture state 和 `scenario_ready`，不再只启动普通 live app。
- 本机 `emulator-5554`（arm64-v8a）真实运行 capture-only Counter scenario：安装、启动、
  control 连接、heartbeat、native `adb exec-out screencap -p` 和 matrix report 均通过；
  artifact `png-589ce43a376f287ae20e1b5de2e9e4114bebbbdb157bbf2042de129d6b6d31d9`，PNG
  尺寸 1280×2856。该文件位于本机临时 probe 目录，不是 CI 或仓库内的持久验收产物。
- 同一 emulator 的 semantics-required Counter scenario 返回
  `unavailable: semantic capture is unavailable for the selected window`；这被保留为真实
  capability 结果，没有用设备截图或 heartbeat 降级成 semantics 通过。
- #147 的内部 `CheckReport` 保留 runner 提供的 `context`：ready/reset generation、
  runtime 自报环境和 `uncontrolled_inputs`。2026-09-29 的历史复核曾发现 matrix CLI 只投影
  status/error/capture artifact IDs；后续 matrix cell report 已保留完整 context/steps，#223
  又补上 cleanup 后读取 runner context 的路径。因此当前 JSON 可以看到 cleanup-finalized mobile
  lifecycle evidence，但这仍不能声称 Android matrix 已提供完整 generation 1→2 或真实三端验收。

## 尚未覆盖

同一冻结快照构建、远程 runner 和完整 macOS+iOS simulator+Android emulator 真实矩阵证据
属于后续 M02 子 PR。
当前 resource pool 只负责单一 supervisor 的调度互斥，不替代 host-shared
DeviceLeaseSession；真实设备竞争仍必须经过 OS lease、fencing 和 runner cleanup。当前还
没有完整 macOS+iOS simulator+Android emulator 运行证据，也没有把当前纯 Rust/无设备 CI
结果写成移动 L2 验收。
