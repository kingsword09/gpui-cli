# M01：Android emulator/device runner adapter（2026-09-28）

状态：in_progress。本切片把 Android runner 接入 `gpui run android` 的非 live 安装/启动路径，
并提供 lease-bound screencap、force-stop 和有界 logcat snapshot；不宣称已完成多 serial
故障矩阵、ABI 运行期诊断、foreground probe 或 scenario check 移动端 driver。

## 已交付

- runner 绑定实际 ADB serial，不使用 AVD 名称替代 serial；安装、force-stop、启动经过
  `run_leased_workload` 的前后 fencing 校验。
- `adb -s <serial> exec-out screencap -p` 通过共享 `CaptureArtifact` 校验 PNG、尺寸、hash、
  系统 UI 和 run id；不经文本管道。
- `adb -s <serial> logcat -d -v threadtime` 有 8 MiB 上限；能取得包 PID 时按 PID 过滤，
  否则保存为 unassigned 并记录原因。
- stop 只 force-stop 本 runner 的 bundle，保留用户 emulator/device 本身。

## 尚未覆盖

PID start identity、ANR/native crash 关联、ABI 不匹配预检、多设备并行/误选、旋转/键盘/后台
切换和完整 M-02 故障矩阵属于后续切片。无真实 Android emulator 时，CI 只验证契约和命令
边界，不记为 L2 通过。
