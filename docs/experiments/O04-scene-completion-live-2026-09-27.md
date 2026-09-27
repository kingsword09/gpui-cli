# O04：真实 macOS scene completion 验收（2026-09-27）

状态：`limited_adopt`。生成的 debug runtime 已在 GPUI frame hook 后上报
`scene_completed`，supervisor 正确保存 scene epoch 和 source/asset revision；
window screenshot 可以因此标记 `freshness.scene=matches`。这不是 GPU present
证明，`presented_frame_id` 继续保持 `null`。AccessKit 未激活时语义树和依赖语义
的 action 仍明确不可用。

## 实验环境

- macOS 15.6.1（24G90）、Apple M2、arm64、Xcode 26.2；
  `rustc 1.97.1` / `cargo 1.97.1`；
- 隔离生成的 macOS-only `SceneEvidence` 项目；
- live session：`live-f27aab46cd5dcc33ec01cd8eafacf30b`；最终运行身份为 build
  `b2`、run `r1`、window `main`；
- 当前运行 revision：source `2`、assets `1`；Screen Recording provider 为
  `macos_screencapture`。

## 实现边界

- live launcher 注入 `GPUI_LIVE_SOURCE_REVISION` / `GPUI_LIVE_ASSET_REVISION`；
  Android 的 `gpui_live.txt` 同样携带两个 revision；
- 生成 runtime 在 `MainView::render` 中注册一个 `Window::on_next_frame` 回调；
  回调执行时递增进程内 scene epoch，并发送 `scene_completed`；
- scene completion 只携带真实 source/asset revision，永远不填充未经 backend
  验证的 `presented_frame_id`；
- source 或 asset revision 未就绪时不发送事件，避免用 `0` 冒充当前场景。

## 真实证据

窗口状态报告：

- `scene_epoch=4`；
- `scene_source_revision=2`、`scene_asset_revision=1`；
- `scene_completed_at_ms` 已记录；
- UI heartbeat `responsive`，app channel `connected`。

app-channel 事件 seq `1718`–`1723` 中，scene epoch `1`、`2`、`3`、`4` 均被当前
run/connection 接受，且每条都记录：

```json
{
  "kind": "scene.completed",
  "source_revision": 2,
  "asset_revision": 1,
  "data": {
    "accepted": true,
    "scene_epoch": 4,
    "presented_frame_id": null,
    "window_id": "main"
  }
}
```

执行：

```text
gpui dev observe --window main --require screenshot --json --timeout 30s
```

结果：

- operation `op-35565-1` 为 `succeeded`；
- observation：`observation-9078c79a90106411d989093a3f172f56`；
- PNG artifact：`obs-9078c79a90106411d989093a3f172f56-window`；
- PNG：47,969 bytes，1536×1055，SHA-256
  `536b42a31ff832b8f5261f6fe33d8cdfb024100f7884a687cbd89a88f8ed4dff`；
- `scene_epoch_before=4`、`scene_epoch_after=4`；
- `freshness.source=current`、`freshness.assets=applied`、
  `freshness.scene=matches`；
- `presented_frame_id=null`，`provider=macos_screencapture`，
  `window_match=pid_single_unnamed`。

PNG 目视包含 `SceneEvidence`、`Clicked 0 times` 和 `Click me`，并非空产物。

## 未完成能力的真实结果

`gpui dev observe --window main --require semantics --json`（operation
`op-35565-3`）仍以 `unavailable` 失败：`provider=gpui-debug-a11y`、
`status=inactive`、`a11y_active=false`、`reason=a11y_inactive`；没有生成空 tree
artifact。

使用仅含 screenshot artifact 的 observation 发起 Counter click（operation
`op-35565-2`）仍被 `stale_observation` 拒绝，原因是 action admission 需要语义
observation 中的窗口 scene epoch 和唯一 target query；没有向 GPUI 投递 click。
这不是 scene reporter 的失败，而是语义树未激活时的正确前置拒绝。

## 结论矩阵

| 能力/断言 | 结果 |
| --- | --- |
| runtime → supervisor `scene_completed` | `adopt`（真实 epoch/revision/connection fencing） |
| screenshot 与 scene revision 关联 | `limited_adopt`，`freshness.scene=matches` |
| GPU present / 屏幕已显示 | 未证明，`presented_frame_id=null` |
| semantics tree | `unavailable`，`a11y_inactive` |
| observation-bound Counter click | 明确拒绝，缺少 semantic target query |

后续仍需在用户控制的 AccessKit 激活环境中重跑语义快照，并在具备语义
observation 后验证真实 click；本切片不自动开启系统权限，也不将 `on_next_frame`
提升为 present fence。
