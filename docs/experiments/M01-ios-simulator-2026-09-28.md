# M01：iOS simulator runner adapter（2026-09-28）

状态：in_progress。本切片把 `IosSimulatorRunner` 接入 `gpui run ios` 的 simulator 路径，
并提供 lease-bound capture、terminate 和有界 simulator log snapshot；不宣称已完成真实
foreground probe、旋转/权限/后台故障矩阵或 scenario check 移动端 driver。

## 已交付

- 明确 UDID 绑定 `RunnerInfo`、`RunRequest` 和 `DeviceLeaseSession`；安装/启动经过
  `run_leased_workload` 的前后 fencing 校验。
- `simctl io <udid> screenshot` 输出由共享 `CaptureArtifact` 校验 PNG、尺寸、hash、系统 UI
  和 run id；设备截图不冒充 scene capture。
- `simctl terminate <udid> <bundle_id>` 只针对本 runner 的 bundle；清理证据保留 simulator
  本身，不自动关闭用户设备。
- `simctl spawn <udid> log show --style json --last 1m` 有 8 MiB 上限，超限记录 truncated。
  当前没有可靠进程启动身份时，日志明确保存为 `assigned_to_run=false`/unassigned。
- `gpui run ios` simulator 安装/启动已走 `IosSimulatorRunner`；physical iOS 仍走独立路径。

## 尚未覆盖

前台 app/status probe、真实 app PID/start identity、live runner 的 evidence 接线、系统弹窗、
键盘、旋转、后台切换、early native crash 和 channel disconnect 故障矩阵属于后续 M01 子 PR。
缺少真实 simulator 时，CI 只验证命令契约与纯 Rust evidence，不记为 L2 通过。
