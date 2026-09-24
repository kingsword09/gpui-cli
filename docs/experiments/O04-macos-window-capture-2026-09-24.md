# O04：macOS window capture provider（2026-09-24）

状态：新增受限的 macOS window screenshot provider，供 `gpui dev observe` 使用。
实现依据 P01 实机记录：锁定 GPUI 的 `Window::render_to_image()` 当前返回
`not implemented for this platform`；`CGWindowList` + `screencapture -l` 曾成功
捕获目标 OS window。

## 行为

- 用受管 app PID、注册窗口标题和 `CGWindowListCopyWindowInfo` 定位当前屏幕上的
  layer-0 窗口；不按进程名或任意全屏窗口猜目标；
- 调用系统 `screencapture -x -o -l <window_number>`，截图写入本次调用的私有临时目录；
- 执行受 observe 总 deadline 限制；PNG 上限 16 MiB，ArtifactStore 再校验 PNG 头、尺寸、
  像素总数和 SHA-256；
- `observe --sync` 在目标 run 连接后先等待对应 `asset_revision` 的 `assets_applied`
  确认；采集结束再次核对资源确认，确认丢失时拒绝本次观察；
- 取消或 deadline 到达时轮询中的 capture helper 会被终止；artifact 按块传输时
  逐块检查 operation 终态，未发布 transfer 会 abort，迟到完成不能改写 cancelled/
  timed_out；
- 采集前后核对 run、window、UI heartbeat 和 tracked input revision/hash；中途变化时
  operation 失败，不发布成功 observation；
- 发布不可变 PNG artifact，记录窗口号/bounds、像素尺寸、前后 scene_epoch、时间、
  provider、实际 run/revision 和 freshness；一致性标为 `best_effort/window`，
  `presented_frame_id` 仍保持 null，scene 改变时 freshness 为 unknown；
- screenshot alias 在 macOS 解析到 `capture.window`；显式要求 `capture.scene`、
  `capture.device` 或 semantics 仍返回 `unavailable`。

## 验证

- macOS 单元测试运行 Swift 枚举 helper，确认源码可编译，并确认无匹配 PID/title 时
  返回明确错误；
- PNG 元数据解析测试覆盖签名、非零尺寸和宽高读取；ArtifactStore 现有测试覆盖 PNG
  大小、像素上限、SHA-256、传输完整性和原子发布；
- 同步观察测试覆盖资源确认未到达时保持 operation pending；
- macOS bounded-helper 测试覆盖取消时终止子进程；
- macOS live GUI 截图需在已授权 Screen Recording 的交互会话中实测。该 provider
  不证明 GPUI scene 读回、present fence、被遮挡内容或语义树。
