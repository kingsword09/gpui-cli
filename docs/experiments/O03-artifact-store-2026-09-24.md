# O03：产物库与有界分块（2026-09-24）

状态：本子 PR 实现并验证了会话产物库、应用分块上传、控制端按
artifact_id 读取和 CLI 原子导出；observe/capture 仍会在后续 O04/O05 接入。

## 已实现

- ArtifactManifest、artifact_begin、artifact_chunk、artifact_end 和
  artifact_abort 进入 dev protocol；
- 原始块固定最多 128 KiB，上传按 (artifact_id, transfer_id, offset) 校验；
  重复发送相同已写入块可以安全重试，内容不同的旧 offset 会被拒绝；
- 产物写入 .gpui/artifacts/<session_id>/ 下的临时文件，只有声明大小、
  SHA-256 和类型校验都通过后才通过 rename 发布；
- PNG 限制为 16 MiB、20 MP；树 JSON 限制为 16 MiB、50,000 个对象节点；
- session 256 MiB、project 2 GiB 配额，project 配额使用 .quota.lock 保护
  并跨 session 计算；同时传输数按 run 限制为 2；
- pinned 和活动引用保护产物，后台周期回收过期文件；重启时清理未完成的
  .part，可恢复已写入但尚未完成元数据发布的文件；
- gpui dev artifact info/get/pin/unpin 通过认证 control channel 操作
  artifact_id，get 在本地临时文件完成、校验哈希后原子导出，并要求显式
  --overwrite 才替换已有文件。

## 证据

- artifact store 单元测试覆盖：不完整传输不可读、块大小和 offset、重试与
  transfer fencing、checksum mismatch、PNG 尺寸、树节点数、session/project
  配额、并行预留、断线清理、pin/活动引用/过期和导出路径；
- app channel 测试覆盖：应用上传后收到 receiving/published ACK，published
  之前控制端不能读取；
- CLI 测试覆盖：多块读取、下载哈希校验、目标文件存在时拒绝覆盖和显式覆盖；
- workspace cargo test 覆盖 145 个 CLI 测试以及协议/集成测试。

## 边界

当前产物库已经能承载截图、语义树和其他观察结果，但还没有把 capture 请求
连接到真实 GPUI scene，也没有 scene_completed 或 presented_frame_id 证据。
这些身份字段必须在 O04/O05 的观察编排接入时写入同一份 artifact metadata；
文件发布成功本身不能表示画面已经呈现。

