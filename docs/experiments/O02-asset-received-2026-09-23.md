# O02：资源 received ACK（2026-09-23）

状态：in_review。本子 PR 将传输层到达与 UI 线程应用分开：runtime 收到并成功写入
asset bytes、或收到 changed/removed 指令后发送 `assets_received`；缓存失效仍由之后的
`assets_applied` ACK 表示。

## 实现

- `assets_received` 携带 `transfer_id`、`asset_revision`、成功路径和失败路径；
- iOS bytes 在 base64 解码并写入临时文件后回报 received，写入/解码失败进入 failed；
- desktop/Android/iOS 的 changed/removed 指令进入 runtime 队列后回报 received；
- supervisor 只记录当前 transfer 的 received 事件，不把它写成 `assets_confirmed`；
- 既有 UI-thread cache invalidation ACK 保持独立，旧 transfer 的 received/applied 都会被
  fencing 拒绝或标记为未接受。

## 边界

received 仍是单帧/单消息路径，没有事务 commit、缺块重传、声明 hash 校验和
`required_loaded`/scene 绑定；后续继续补齐资源事务和分块传输。
