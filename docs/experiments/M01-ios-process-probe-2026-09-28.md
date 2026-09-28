# M01：iOS simulator process probe（2026-09-28）

状态：in_progress。本切片补齐 simulator app 的 PID 与 bounded log attribution；没有真实
simulator 故障矩阵时，不宣称完成 L2 移动设备验收。

## 已交付

- 通过 `simctl spawn <udid> id -u` 确定用户 domain，再探测
  `launchctl print gui/<uid>/<bundle>`，并以 `system/<bundle>` 作为 fallback。
- launchd service 输出中的 PID 经过最多 2 秒的有界等待，写入 `LaunchEvidence.process.pid`；
  launch success 不因 app early crash 或 service 尚未出现而伪报 process exited。
- simulator log snapshot 在有 PID 时附加 `processID == <pid>` predicate，并把 PID 写入
  `LogEvidence`；无 PID 时保留完整 bounded snapshot 为 unassigned。
- 由于 simulator 当前没有被验证的跨版本 process start-time 来源，`start_token_sha256`
  保持为空；PID-only 证据不升级为 start identity 或 foreground 证据。

## 尚未覆盖

foreground app/status probe、process start identity、系统弹窗、旋转/键盘/后台切换、early
native crash/channel disconnect fault matrix 和真实 simulator L2 运行属于后续 M01 子 PR。
