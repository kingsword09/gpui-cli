# M01：Android process identity（2026-09-28）

状态：in_progress。本切片把 Android launch/log evidence 从“PID 存在”提升为有界的
进程启动身份证据；没有真实 Android emulator 时，不宣称完成 L2 设备验收。

## 已交付

- `adb -s <serial> shell pidof -s <bundle>` 得到 PID 后，读取同一 serial 的
  `/proc/<pid>/stat` start-time ticks 和 `/proc/sys/kernel/random/boot_id`。
- process token 只在内存中组合，`LaunchEvidence`/`LogEvidence` 仅保存 SHA-256；token 同时
  绑定 PID、进程启动 tick 和（可取得时）设备 boot identity，避免 PID 重用被误归属为同一 run。
- `am start` 后最多等待 2 秒取得 identity；进程早退、尚未出现或 proc 读取竞态均保留为
  process identity unavailable，launch 不被伪报为已退出。
- logcat 仅在 collection time 取得 process identity 时标记 `assigned_to_run=true`，并记录
  PID 与 token hash；否则仍保存 bounded logcat 为 unassigned 并记录原因。

## 尚未覆盖

early native crash/ANR 的结构化关联、iOS foreground/process probe、旋转/键盘/后台切换、
channel disconnect 和完整 simulator/emulator fault matrix 属于后续 M01 子 PR。
