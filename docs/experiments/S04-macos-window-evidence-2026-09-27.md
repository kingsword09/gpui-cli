# S04：真实 macOS window evidence（2026-09-27）

状态：`limited_adopt`。真实 macOS live build → run → window heartbeat →
`capture.window` → ArtifactStore → PNG 下载与目视核验已通过；语义树、scene
readback、present fence、真实动作注入和遮挡实验没有被冒充为通过。

## 环境与实验项目

- macOS 15.6.1（24G90）、Apple M2、arm64、Xcode 26.2；
  `rustc 1.97.1` / `cargo 1.97.1`；主显示器 1920×1080 @ 60 Hz；
- 使用隔离临时目录生成 macOS-only `WindowEvidence` 项目，没有修改用户项目；
- live session：`live-2e2b47a7b9cf7cae5e4ec1595c5ebe48`；最终成功结果为 build
  `b4`、run `r3`、window `main`；
- Screen Recording 权限可用，capability `capture.window` 的 provider 为
  `macos_screencapture`，scope 为 `window`，consistency 为 `best_effort`。

## 真实步骤与结果

### 1. 窗口与 runtime 健康

`gpui dev status --json` / `gpui dev windows --json` 报告：

- app process `running`，channel `connected`；
- `main` 窗口 `foreground=true`、`ui=responsive`、注册逻辑尺寸 800×600；
- `assets_confirmed=true`，当前 `source_revision=4`、`asset_revision=1`。

### 2. 同步 window observe

执行：

```text
gpui dev observe --sync --window main --require screenshot --json --timeout 60s
```

结果：

- operation `op-7826-6` 为 `succeeded`；
- observation：`observation-8bd9bb9a2d6d2d391d2999d39c011096`；
- artifact：`obs-8bd9bb9a2d6d2d391d2999d39c011096-window`；
- PNG：48,604 bytes，SHA-256
  `4ddc9486a4a7f3e31cf5bc76d651d14c84c34e94d74b2dde59541d1b85213d04`；
- provider 返回的 OS window bounds 为 1536×1055，PNG 也是 1536×1055，
  `orientation=landscape`、`includes_system_ui=false`；
- `freshness.source=current`、`freshness.assets=applied`、
  `freshness.scene=unknown`，`presented_frame_id=null`；
- `window_match=pid_single_unnamed`、`window_number=13646`，符合 macOS
  不暴露 `CGWindowName` 时的单窗口 PID 回退规则。

artifact 下载到项目目录之外后，`file` 确认其为非空 RGBA PNG。目视内容包含
`WindowEvidence`、`Clicked 3 times` 和 `Click me`，因此不是空 PNG 或旧 metadata
伪装的新结果。

### 3. 语义与动作边界

执行 `gpui dev observe --window main --require semantics --json`，operation
`op-7826-8` 明确失败：

```json
{
  "code": "unavailable",
  "provider": "gpui-debug-a11y",
  "status": "inactive",
  "a11y_active": false,
  "reason": "a11y_inactive"
}
```

没有生成 tree artifact，也没有把空树当成成功。对仅含 window screenshot 的
observation 发起 `click` 时，operation `op-7826-7` 以
`stale_observation` 拒绝，原因是该 observation 没有选定窗口的 scene epoch；
因此没有向 GPUI 投递 click，也没有声称 Counter 发生了业务变化。

另一次在 app 重启后使用旧 r1 observation 的 click 被 `stale_observation` 拒绝，
details 同时包含 `observation_run_id=r1` 与 `current_run_id=r2`，证明旧运行结果
不会接管新窗口。

## 实验中的版本与产物边界

第一次非同步截图发生在 runtime 重启附近，得到 149×179 的 best-effort PNG，且
随后 source/run 已变化；该结果没有被纳入成功证据。把下载文件放在被 watcher 监视
的项目根目录也会产生新的 source revision，触发一次真实重启。之后将 evidence 输出
放到项目外并重新执行 `observe --sync`，才得到上面 build `b4` / run `r3`、
`freshness.source=current` 的最终结果。

这同时验证了两条边界：观察结果必须绑定当前 run/source revision；实验产物不能被
当作当前 UI 的源码输入。CLI 不会把旧截图换上新版本号。

## 结论矩阵

| 能力/断言 | 结果 | 证据 |
| --- | --- | --- |
| macOS `capture.window` | `limited_adopt` | 成功 PNG artifact、尺寸/哈希/目视内容 |
| Screen Recording permission | 可用 | `capture.window` 成功，provider 为 `macos_screencapture` |
| `semantics.read` | `unavailable` | `a11y_inactive`，无 tree artifact |
| screenshot observation 驱动 click | 明确拒绝 | 无 scene epoch，未 dispatch |
| old run/old observation fencing | 通过 | r1 → r2 的 `stale_observation` |
| same-scene / GPU present | 未证明 | `freshness.scene=unknown`、`presented_frame_id=null` |
| 透明 overlay、系统遮挡、人工输入污染 | `not_run` | 尚无真实 provider/受控注入证据 |

下一步需要在用户控制的 macOS accessibility/AccessKit 激活环境中重新运行
`semantics.read`，再补 scene 绑定和真实 action admission；本实验不自动开启系统
权限，也不以截图或 OCR 代替语义树。
