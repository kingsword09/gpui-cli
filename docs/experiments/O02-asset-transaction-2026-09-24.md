# O02：资源事务 begin/commit（2026-09-24）

状态：in_review。本子 PR 为资源变更增加批次边界：CLI 先声明一组变更，发送内容或
删除指令，最后提交；runtime 在 commit 前不把变更交给 UI 线程。

## 实现

- 新增 `assets_begin`，携带 `transfer_id`、目标 `asset_revision`、变更文件 hash 和删除项；
- 新增 `assets_commit`，要求 transfer identity 与 begin 相同；
- asset-only 快路径和 reconnect manifest 对账都按 begin → 内容/删除 → commit 发送；
- runtime 在 begin/commit 之间暂存 `AssetEvent`，部分传输不会提前失效 GPUI 缓存；
- begin 的增量 manifest 更新 runtime 的 desired hash/transfer，commit 后既有
  `assets_received` 与 UI-thread `assets_applied` 继续保持独立。

## 边界

本子 PR 仍未实现桌面/Android 读取阶段的声明 hash 校验、分块 offset/重传、`required_loaded`
或 `scene_epoch` 绑定；iOS bytes 的校验和临时文件原子提交属于后续完整性子 PR。这些
属于后续资源完整性和观察一致性子 PR。
