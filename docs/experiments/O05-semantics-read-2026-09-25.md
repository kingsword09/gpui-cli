# O05：语义快照第一切片（2026-09-25）

状态：已实现受控 `semantics.read` 请求链路与显式 logical-id adapter；GPUI 语义树是否可用仍由运行时的
无障碍激活状态决定，不能把普通环境中的 `null` 树解释为成功。

## 实现范围

- app-channel proto 2 新增 `semantics_query` / `semantics_result`；请求包含
  `request_id`、`window_id` 和有界 `max_bytes`。
- 生成 runtime 保存 `window_id -> AnyWindowHandle`，网络线程只入队，UI 线程通过
  `cx.update_window(...)` 调用 `Window::is_a11y_active()` 和
  `Window::debug_a11y_tree_json()`。
- supervisor 只接受当前 run、当前 app-channel connection、当前已登记窗口和当前
  在途 request 的回复；迟到回复、旧连接回复、关闭窗口回复不会改写当前结果。
- runtime hello 携带 `semantics.read` 后，当前连接能力会动态标记为
  `available=true`；这表示 provider 支持受控读取，不表示每一帧都有可用语义树。
- runtime hello 另携带 `semantics.logical_id`；模板提供显式
  `declare_logical_id(element_id, logical_id)`，只在 debug adapter 中把声明映射写入节点。
  冲突声明或同一树重复匹配会返回明确 unavailable，不把 `.id()` 自动提升为稳定选择器。
- `ready` 树先验证 JSON，再以 `ArtifactKind::Tree` 写入会话 ArtifactStore，沿用
  SHA-256、分块、大小和节点数校验；observe 结果同时携带 artifact 引用、节点数、
  provider 和采集时间。
- `inactive`、`unavailable`、`too_large`、非法 JSON 均不能生成成功的语义产物。

## 有界策略

- runtime 单次语义树读取上限为 256 KiB，低于 app-channel 1 MiB frame 上限；树产物
  仍受 ArtifactStore 的 16 MiB / 50,000 节点硬上限保护。
- UI 请求队列上限为 16；supervisor 的在途语义读取按窗口单飞，发送失败会转成
  `unavailable`，不会无限等待。
- 语义树与 macOS `screencapture` 当前不是同一 scene 的证明；带截图的结果继续标记
  `freshness.scene=unknown`，不声称 `same_scene` 或 GPU present。

## 验证

- `semantics_read_roundtrip_is_run_and_window_bound_and_publishes_tree_artifact`：真实
  TCP app-channel 往返、run/window/connection 身份校验和 tree artifact 发布。
- `semantics_reply_is_bound_to_the_window_connection_and_size_limit`：迟到连接与大小
  上限拒绝。
- 生成的桌面模板实际通过 `cargo check --workspace --all-targets`。
- workspace 全量测试通过（163 个单测及全部集成测试）。

### 真实 macOS live 验收

在隔离生成的 `O05 Semantics` 桌面项目中完成 build → run → channel → window/UI
heartbeat → `gpui dev observe --require semantics` 闭环：

- status 报告 `semantics.read.available=true`、provider 为 `gpui-debug-a11y`；
- app-channel 事件 `semantics.read` 被接受，`status=inactive`、`a11y_active=false`、
  `reason=a11y_inactive`；
- observe 以 `unavailable` 失败，没有生成树 artifact，也没有把空树当作成功。

## 未覆盖

显式 logical-id bridge 已实现，但真实 macOS 无障碍激活环境尚未重新验收；仍未实现
分页/投影以外的 bounds 适配、scene readback 或 present fence。GPUI debug 导出的
`a/b/c` 等临时节点引用不能跨观察复用，也不作为稳定自动化选择器。
