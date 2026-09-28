# M01：移动端 runner 契约与证据模型（2026-09-28）

状态：in_progress。本切片交付 iOS simulator / Android emulator 共用的 runner trait、
run identity、lease fencing 边界和结构化证据类型；iOS simulator 与 Android adapter 已在
后续子 PR 接入，完整故障矩阵仍待完成。

## 契约

- `src/runner/mobile.rs` 定义 `MobileRunner`：`prepare`、`launch`、`capture`、
  `collect_logs`、`stop_owned` 均显式接收 `DeviceLeaseSession`。
- `RunIdentity` 绑定 run、project、stable device、lease session 和 fencing token；raw token
  只留在内存，序列化证据只保存 SHA-256 指纹。
- `run_leased_workload` 在外部 workload 前后验证当前 lease，旧 token、不同 device 或不同
  session 不能继续执行。
- `CaptureArtifact` 只接受有界 regular PNG，记录 provider、hash、尺寸、方向、系统 UI 和
  前台应用声明；截图不是 scene capture。
- `EvidenceLog` 以单调序号记录 prepare/install/launch/capture/log/channel/process/stop，
  channel disconnect 可以是 `unknown`，不会被推断成 process exited。
- `LogEvidence` 明确 `assigned_to_run`；无法归属的原生日志必须保存为 unassigned。

## 验证

- 预置 identity 的 fencing token 不会出现在 JSON 证据中。
- identity 不匹配时，外部 workload closure 在执行前不会被调用。
- channel disconnect 的证据保持 `unknown`，事件序号稳定递增。
- 非 PNG 被拒绝；PNG header、尺寸、hash、系统 UI 和前台 app 元数据被记录。

## 尚未覆盖

真实 iOS 前台/进程身份、旋转/后台切换、Android PID start identity 和 simulator/emulator
故障矩阵尚未完成；当前纯 Rust 与无设备 CI 测试不宣称有完整 L2 移动设备证据。平台边界见
[M01 iOS simulator](M01-ios-simulator-2026-09-28.md) 与
[M01 Android runner](M01-android-runner-2026-09-28.md)。
