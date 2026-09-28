# S04：desktop check runner 接线（2026-09-28）

状态：`in_progress`。本切片把 S04 核心接入真实 desktop preview/control session；移动端、
视觉基线和完整矩阵仍未完成。

## 已实现

- 新增 `gpui check --scenario <id> --target desktop [--file ...] [--json]`。命令先执行
  S01 静态校验，再启动独立 `gpui preview` 子进程；不会读取 Live snapshot 或附着用户已有
  session。首次运行会先生成缺失的 `Cargo.lock`，避免 preview 构建期间 watcher 将 Cargo
  自动写出的 lockfile 误判为输入 supersession。
- runner 等待 control registration 和对应 `scenario_ready`，向当前 preview 发送 fenced
  `scenario.reset`，等待 reset result 与新 generation ready，再建立初始 observation。
- `observe` 通过现有 operation/control API 请求真实 semantics/window capture；query 使用
  bounded 200 节点投影，映射 logical_id、role/name、value、enabled/focused 和 bounds/clip。
  bounds 与 clip 的几何关系只用于保守的 visible/not_clipped 判断。
- click/type_text/key/scroll 复用已有 observation-bound `Command::Act`，只允许 logical_id
  selector；role/name 目前报告 `selector_not_routable`，不猜测 logical_id。
- `wait_for` 使用 deadline-bounded observation polling；`no_runtime_errors` 只看 reset 后
  新增 runtime issue；动作/observe/control 断连和 operation unknown 继续保留原终态语义。
- check 结束后总是 kill/wait 隔离 preview 子进程，cleanup 错误单独进入报告。

## 验证

- 新增纯 Rust 测试覆盖 bounds 可见性/裁剪映射和不可路由 role/name selector。
- 定向 check tests、fmt、clippy 已通过；完整 workspace 门槛仍在本 PR 中运行。

## 未覆盖

- `screenshot_matches` 尚未读取/比较 baseline；即使 PNG 已采集也返回 `inconclusive`，不自动
  通过。
- check 当前只支持 desktop；移动端设备租约、真实模拟器 runner、矩阵调度和 20 次证据尚未
  接入。
- capture report 目前保留 capture kind，artifact 下载/报告引用和视觉 diff 仍由后续切片补齐。
- 本地没有真实 macOS 图形会话时，observe 会明确返回 unavailable/inconclusive；不以模板
  编译通过替代窗口运行证据。
