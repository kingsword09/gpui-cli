# O01：窗口注册与 UI heartbeat（2026-09-23）

状态：in_progress。本子 PR 在 F02 proto 2/runtime 基础上补 supervisor 侧的 run-scoped
窗口 registry、UI heartbeat 和 `gpui dev windows` 查询；不把进程存活或网络线程收包
当作 UI 响应。

## 实现

- 每个 `(run_id, window_id)` 保存窗口标签、逻辑尺寸、scale、前台状态和生命周期；
- 前台窗口每约 1 秒最多一个在途 `probe_ui`，请求通过 app channel 发到 runtime UI 泵；
- 3 秒未收到同一连接、同一 request_id 的结果时记录 `unresponsive/probe_timeout`；
- 迟到、未知窗口、错误连接的回复只写 `accepted=false` 事件，不覆盖当前状态；
- app 关闭、退出或通道断开分别转为 `closed/unavailable` 或 `unknown`；
- `gpui dev status` 返回当前 run 的窗口/UI 汇总，新增 `gpui dev windows` 返回窗口明细；
- `gpui dev windows --json` 使用既有 schema 2 envelope 和 session/request 身份校验。

## 验证

    cargo fmt --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs

测试覆盖单飞 probe、超时、迟到回复、关闭窗口、真实 app-channel heartbeat 往返和
run-scoped windows 查询。当前未宣称 GPU 呈现、scene 完成或截图能力；那些事实仍由
后续 O03/O05 负责。
