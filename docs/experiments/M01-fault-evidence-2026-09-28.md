# M01：mobile fault evidence boundary（2026-09-28）

状态：in_progress。本切片把 launch 后的 process/channel evidence 接入统一事件日志，先
固定 early crash、process exit、channel disconnect 和未知状态的判定边界；没有真实
simulator/emulator 故障运行时，不宣称完成 L2 fault matrix。

## 已交付

- Android/iOS runner 在 launch 成功后追加 `process` 与 `channel` boundary events，保持事件
  序号单调，并保留 launch evidence 本身。
- process PID 缺失、进程退出但没有 exit code、或平台只返回 unavailable 时均为 `unknown`；
  不把“没有 PID”写成 process exited。
- 明确的非零 exit code 才记录 process `failed`；channel `not_established` 或
  `disconnected` 始终是 `unknown`，不会反向推断 native crash 或 process exit。
- process/channel 事件的 run、device、lease identity 仍沿用同一 `EvidenceLog` fencing
  边界，便于后续矩阵 cell 按事件序列收集证据。

## 尚未覆盖

真实 early native crash/ANR 采集、系统弹窗、旋转/键盘/后台切换、foreground probe、
channel 重连和 simulator/emulator fault matrix 属于后续 M01/M02 子 PR。
