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
  覆盖原本的 passed，cell deadline 传入 adapter；当前 harness 仍按序执行，保留后续并行
  executor 的相同 report contract。
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
- check report 现在保留 runner 提供的 `context`：ready/reset generation、实际环境和
  `uncontrolled_inputs`；Android reset probe 的 generation 1→2 不必再只从 event journal
  反查。

## 尚未覆盖

同一冻结快照构建、远程 runner 和完整 macOS+iOS simulator+Android emulator 真实矩阵证据
属于后续 M02 子 PR。
当前 resource pool 只负责单一 supervisor 的调度互斥，不替代 host-shared
DeviceLeaseSession；真实设备竞争仍必须经过 OS lease、fencing 和 runner cleanup。当前还
没有完整 macOS+iOS simulator+Android emulator 运行证据，也没有把当前纯 Rust/无设备 CI
结果写成移动 L2 验收。
