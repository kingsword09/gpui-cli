# M03：host device lease/fencing 原语（2026-09-28）

状态：in_progress。本切片交付 runner 可复用的 host-level lease，不宣称已经接入
iOS/Android 安装、启动或矩阵调度。

## 契约

- src/runner/lease.rs 使用跨平台 OS 文件锁保持设备独占；owner JSON 保存
  device_id、session_id、PID、process_start_token、fencing_token、获取时间和 heartbeat。
- lock path 和 owner path 位于项目的 .gpui/leases/，device/session/start token 只接受
  有界安全标识，路径组件拒绝 symlink。
- 第二个 owner 在锁被占用时得到 device_busy，并可读取当前 owner 诊断；不会用 TTL、
  “PID 看起来不存在”或宽泛删除抢占设备。
- heartbeat 和 release 都重新核验 device/session/fencing token。owner 文件被替换后，
  原 lease 得到 fencing_lost，不能删除新 owner 的记录。
- Drop 只在 token 仍匹配时清理 owner，并释放 OS lock；正常进程退出由 OS 释放文件锁。

## 验证

- 两个 owner 争用同一 device id，第二个得到 busy，释放后下一个 owner 成功。
- heartbeat 更新成功；篡改 fencing token 后 heartbeat/release 都拒绝，后续 owner 仍可取得锁。
- Unix symlink lock path 被拒绝。

## 尚未覆盖

Lease 仍未接入 device inventory、安装/启动/截图 runner；heartbeat 调度、设备状态变化和
M01/M02 的真实 simulator/emulator 故障矩阵属于后续切片。
