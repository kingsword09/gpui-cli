# O04：真实 macOS live observe 验收（2026-09-25）

状态：best_effort/window 通过；scene readback、语义树和 present fence 仍未实现。

## 环境与步骤

- 在隔离目录 `/tmp/gpui-o04.VLPh7W` 生成 macOS-only `O04 Counter` demo；未修改仓库或
  用户项目；
- 启动 `gpui run desktop --live`，从独立 control CLI 查询 `status`、`operation get`
  和 `artifact get`；
- 首次依赖编译期间提交 `observe --sync --require screenshot --timeout 60s --async`，
  operation `op-30198-1` 在总 deadline 到达后为 `timed_out`，没有伪造 observation；
- 重启 live supervisor 以加载窗口标题回退 provider，再次提交 120s 同步观察。

## 结果

运行状态确认：

- run `r1`，build `b1`，PID `51946`，channel `connected`，process `running`；
- window `main`，标题 `O04 Counter`，UI `responsive`，`assets_confirmed=true`；
- capability `capture.window` 为 `macos_screencapture`，scope `window`，consistency
  `best_effort`；scene/device/semantics 仍明确 unavailable。

成功观察：

- operation `op-51799-1` 为 `succeeded`；
- artifact `obs-e03f88e6693c761694d130f4efd738e2-window`，48,323 bytes，SHA-256
  `1c3c7d85f05d4820649fe639c3a46ce117918d5b40d91ce093ee84136bee9e41`；
- `window_match=pid_single_unnamed`：macOS `CGWindowName` 为空，但该 PID 只有一个可见
  layer-0 窗口，因此安全回退；
- PNG 为 1536×1055，逻辑窗口为 800×600，scale 1.0，orientation `landscape`，
  `includes_system_ui=false`，`freshness.source=current`，`freshness.assets=applied`；
- 导出的 PNG 目视包含 `O04 Counter`、计数文本和 `Click me` 按钮，证明 artifact 是
  真实目标窗口内容，不是空 PNG。

## 限制与结论

本次证明了真实 macOS live build → run → app channel → window/UI/asset confirmation →
OS window screenshot → ArtifactStore 校验/导出的闭环。由于 generated template 没有
自动 scene reporter，`scene_epoch=0`、`freshness.scene=unknown`、
`presented_frame_id=null`；这次结果不能用于 same_scene、GPU present 或语义断言。
