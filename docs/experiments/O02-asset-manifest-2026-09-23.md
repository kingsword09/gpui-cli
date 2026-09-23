# O02：资源 manifest/hash 对账与重连补发（2026-09-23）

状态：in_progress。本子 PR 为 live asset channel 增加当前资源 manifest 和连接级
reconciliation，解决“socket 曾经写成功”不能证明重连后的 app 已拥有同一组资源的问题。

## 实现

- supervisor 保存当前 `asset_revision` 及按规范化路径排序的 SHA-256 manifest；
- 新连接先收到 `asset_manifest`，runtime 以自己已经完成 UI 线程缓存失效的 hash 集合
  计算 `present`、`missing`、`stale`、`removed`，再发送 `assets_reconciled`；
- supervisor 按 connection id 定向补发 changed 数据/事件和删除事件；Android 先重新写入
  app 私有文件，iOS simulator 重新发送 asset bytes，desktop 重新触发缓存失效；
- 当前 manifest 只在新连接上触发对账，正常热更新仍沿用已有 changed/removed 快路径；
- 对账补发之后仍通过原有 `assets_applied` ACK 判定 runtime 是否完成缓存失效，旧 revision
  的消息不能确认当前 desired revision。

## 边界

manifest 目前是单帧 bounded DTO，尚未拆成带 `transfer_id` 的多块事务；也没有把
`received`、`cache_invalidated`、`required_loaded` 三种状态完全分离。超大 manifest 会被
拒绝，后续需要转入 O03 的有界分块/产物传输设计。

## 验证

- 协议 manifest/reconciliation round-trip；
- app channel 新连接 manifest 下发与 accepted reconciliation 队列；
- runtime manifest parser、missing/removed 对账以及已有 asset ACK 测试；
- workspace 全量测试和生成项目 debug/release/`gpui-dev` 检查。
