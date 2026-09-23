# O02：资源显式删除（2026-09-23）

状态：in_progress。本子 PR 把资源删除从“读取失败后跳过”改成协议内的
`asset_removed` 事件，并让删除与资源修改一样进入 runtime 的 UI 缓存失效 ACK。

## 实现

- watcher 使用前后输入 manifest 的 SHA-256 对账，生成精确的 `changed` 和 `removed`
  路径集合；删除目录时不会只看到目录事件而漏掉子文件；
- 新增 `asset_removed` server message，携带 `path` 和 `asset_revision`；
- desktop 直接广播删除，iOS simulator 在 UI 线程删除临时资源文件，Android 通过
  `run-as ... rm -f files/<asset>` 删除 app 私有文件；
- 删除成功后和修改一样驱逐 Embedded/Path 缓存，并由 `assets_applied` 回报；删除失败
  不会伪造成功 ACK，整批转入重建路径；
- Android 删除路径拒绝绝对路径、`.`、`..` 和反斜杠，避免把资源路径解释成 app 私有目录外的目标。

## 边界

本子 PR 仍没有实现断线后的 manifest 重放/对账，也没有把 ACK 扩展为
`received`、`required_loaded` 和 `transfer_id` 三个独立维度；这些属于后续 O02 子 PR。
`assets_applied` 仍只证明 runtime 完成了本地文件处理和缓存失效，不证明 GPU 已呈现删除
后的 scene。

## 验证

- `AssetDelta` 覆盖新增、内容变化和删除；
- 协议和模板 runtime 覆盖 `asset_removed` wire tag 及 UI 队列；
- Android 私有文件目标覆盖路径逃逸拒绝；
- workspace tests 与生成项目测试继续作为合并门槛。
