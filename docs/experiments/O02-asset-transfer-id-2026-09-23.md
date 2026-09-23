# O02：资源 transfer identity（2026-09-23）

状态：in_progress。本子 PR 将资源传输从单一 `asset_revision` 扩展为
`transfer_id + asset_revision`，避免同一 revision 的重连补发、旧连接消息和新一轮
传输之间互相确认。

## 实现

- `transfer_id` 贯穿 `asset_manifest`、`asset_changed`、`asset_data`、`asset_removed`、
  `assets_reconciled` 和 `assets_applied`；
- supervisor 为每次 manifest 发布生成有界、可记录的传输 ID，并只接受当前 manifest
  的 reconciliation/ACK；
- runtime 按 `(transfer_id, asset_revision)` 聚合 UI 线程处理结果，旧 transfer 的迟到
  事件不能更新当前已应用 hash 集合；
- reconnect 补发和正常 asset-only 快路径使用同一个 transfer identity，事件日志也记录
  对应 ID。

## 边界

当前 transfer 仍是一组单帧资源消息，没有 `assets_begin`/`assets_commit`、块 offset、
声明大小和最终 hash 校验；这些属于 O02 后续事务化以及 O03 有界分块传输。transfer ID
目前用于 fencing/审计，不代表 GPU scene 已经呈现新资源。

## 验证

- shared protocol transfer identity round-trip；
- supervisor manifest 下发、reconciliation 及 ACK 路径；
- generated runtime transfer-aware parser、分组 ACK 和 debug/release 测试。
