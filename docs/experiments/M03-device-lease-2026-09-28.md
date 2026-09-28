# M03：host device lease/fencing 原语（2026-09-28）

状态：in_progress。本切片把 host-level lease 接入 iOS/Android 的非 live 与 live
安装、启动、Android 配置/资源写入，以及平台截图原语；不宣称已经接入 scenario
check 或矩阵调度。

## 契约

- src/runner/lease.rs 使用跨平台 OS 文件锁保持设备独占；owner JSON 保存
  device_id、session_id、PID、process_start_token、fencing_token、获取时间和 heartbeat。
- 默认 lock path 和 owner path 位于主机共享的用户缓存/运行时目录；`GPUI_DEVICE_LEASE_DIR`
  可为隔离 supervisor 指定目录。不同 project root 的同一设备 ID 仍争用同一 OS lock。
- device/session/start token 只接受有界安全标识，设备 ID 会进行跨平台文件名编码，路径组件拒绝
  symlink。
- 第二个 owner 在锁被占用时得到 device_busy，并可读取当前 owner 诊断；不会用 TTL、
  “PID 看起来不存在”或宽泛删除抢占设备。
- heartbeat 和 release 都重新核验 device/session/fencing token。owner 文件被替换后，
  原 lease 得到 fencing_lost，不能删除新 owner 的记录。
- `DeviceLeaseSession` 每 10 秒后台 heartbeat；每个外部移动端 workload 在执行前后再次校验
  fencing token。Drop 只在 token 仍匹配时清理 owner，并释放 OS lock；正常进程退出由 OS
  释放文件锁。
- `simctl io screenshot`、`devicectl device screenshot` 和 `adb exec-out screencap -p` 保持
  PNG 为二进制输出，并拒绝通过符号链接写入目标文件；调用方可用同一个 lease session 包裹
  capture。`gpui device capture <id> --output <path>` 已提供实际的选设备、lease、capture、
  release 路径，并要求在 GPUI 项目内运行。

## 验证

- 两个 owner 争用同一 device id，第二个得到 busy，释放后下一个 owner 成功。
- heartbeat 更新成功；篡改 fencing token 后 heartbeat/release 都拒绝，后续 owner 仍可取得锁。
- Unix symlink lock path 被拒绝。
- 不同项目根的同一 Android TCP serial 仍争用同一 host lock，且 lock 文件名不依赖 Windows
  不安全的 `:` 字符。
- session 在 fencing 被替换后会在外部 workload 执行前拒绝调用。
- 全 workspace 测试、fmt、clippy、设计文档检查和 diff 检查覆盖本切片。

## 尚未覆盖

Lease 仍未接入 device inventory 的重连状态机、scenario check 的移动端 driver、截图 artifact
 manifest 或 M01/M02 的真实 simulator/emulator 故障矩阵；这些属于后续切片。`gpui run` 的
 非 live 命令在启动完成后释放 lease，因为它当前不监督 app 进程；live 命令则在整个 live
 session 内持有 lease。
