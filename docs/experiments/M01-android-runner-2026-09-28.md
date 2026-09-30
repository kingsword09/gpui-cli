# M01：Android emulator/device runner adapter（2026-09-28）

状态：in_progress。本切片把 Android runner 接入 `gpui run android` 的非 live 安装/启动路径，
并提供 lease-bound screencap、force-stop、有界 logcat snapshot 和 best-effort display metadata
probe；不宣称已完成多 serial 故障矩阵、ABI 运行期诊断或 scenario check 移动端 driver。

## 已交付

- runner 绑定实际 ADB serial，不使用 AVD 名称替代 serial；安装、force-stop、启动经过
  `run_leased_workload` 的前后 fencing 校验。
- `adb -s <serial> exec-out screencap -p` 通过共享 `CaptureArtifact` 校验 PNG、尺寸、hash、
  系统 UI 和 run id；不经文本管道。
- 截图后通过 `wm size`、`wm density`、`dumpsys input` 和
  `dumpsys activity activities` 补充像素尺寸对应的逻辑 viewport、density-derived `scale_milli`、
  方向和当前 resumed/focused activity 的目标包名；包名只在明确的前台标记上匹配。
- display probe 或厂商输出不可用时保留字段为 unknown，并在 check screenshot evidence 中报告
  `mobile_environment_metadata_unavailable`；不从 PNG 像素尺寸猜 DPI 或逻辑尺寸。
- `adb -s <serial> logcat -d -v threadtime` 有 8 MiB 上限；能取得经 `/proc` 验证的包进程
  身份时按 PID 过滤并记录 start token hash，否则保存为 unassigned 并记录原因。
- stop 只 force-stop 本 runner 的 bundle，保留用户 emulator/device 本身。

Android 进程身份的 probe 细节与边界见
[M01 Android process identity](M01-android-process-identity-2026-09-28.md)。

## 尚未覆盖

ANR/native crash 关联、ABI 不匹配预检、多设备并行/误选、真实设备上的前台切换与
viewport/DPI 变体、旋转/键盘/后台切换和完整 M-02 故障矩阵属于后续切片。无真实 Android
emulator 时，CI 只验证契约和命令边界，不记为 L2 通过。
