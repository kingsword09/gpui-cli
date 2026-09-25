# O06：语义查询、分页与绑定（2026-09-25）

状态：查询与变化摘要切片已实现。查询只读、绑定不可变 observation，并且不把 GPUI 临时节点
引用或 `.id()` 猜成稳定 `logical_id`。

## 实现范围

- 新增 `gpui dev query --observation OBSERVATION`，支持 `node-ref`、`id`、role、
  name、parent、字段投影、limit 和 cursor。
- control API 新增 `query` 命令；服务端从成功 operation 的 observation 结果定位
  `ArtifactKind::Tree`，校验 artifact 的 SHA/类型/run 身份后再读取和查询。
- 查询字段以 GPUI debug tree 的真实内容映射：role 来自 `aria.role`，name 来自
  `aria.name`/`aria.label`，children/parent 来自树关系；bounds、clip_bounds、
  enabled、focused 等未导出的字段返回 null 并列入 `unsupported_fields`。
- `logical_id` 只有 runtime 明确导出该字段时才可查询；当前生成模板没有该字段，
  请求明确返回 `unsupported_selector`，不把 `element_id` 或 `a/b/c` 当作稳定 ID。

## 有界分页

- 单页最多 200 个节点、序列化响应最多 128 KiB。
- cursor 绑定 artifact SHA、筛选条件、字段投影和 limit；跨 observation、跨 artifact
  或修改筛选条件都会返回 `invalid_cursor`。
- 大单节点无法放进单页时返回 `query_result_too_large`，不静默截断字段。
- 新增 `gpui dev diff --before BEFORE --after AFTER` 与 control `diff`；比较唯一
  `logical_id` 的节点字段和稳定子节点关系，返回 added/removed/changed/unchanged。
- diff 结果显式返回两侧 observation/artifact/run 身份；缺失或重复 ID 的不稳定子树只返回
  `subtree_replaced`，不按 `node_ref` 配对；跨 run 比较标记 `same_run=false`。
- diff 与 query 共用 200 条/128 KiB 上限，完整类别计数在 `summary`，截断数量在 `omitted`。

## 验证

- query/diff 单元测试覆盖 role/name/parent/投影、logical_id 诚实失败、cursor 绑定、稳定
  ID 变化、临时 node_ref 重排、不稳定子树和结果上限。
- 实际 app-channel semantics roundtrip 测试随后通过 control API 查询刚发布的 tree
  artifact，验证 observation/artifact/run 绑定。
- control 集成测试通过两个发布的 tree artifact 验证 diff 路由、run 绑定和字段变化。
- O05 真实 macOS 环境仍为 `a11y_inactive`；因此没有把查询空树伪装成有节点结果。

## 未覆盖

稳定 logical_id 的 GPUI/runtime 导出、虚拟列表跨帧重定位和大节点独立 artifact
仍属于后续 O05/O06/S 场景切片；当前 diff 对缺少稳定 ID 的区域保持诚实降级。
