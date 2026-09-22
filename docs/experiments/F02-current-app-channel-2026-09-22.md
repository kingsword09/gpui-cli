# F02：当前 app-channel proto 2 与窗口/UI probe 消息（2026-09-22）

状态：in_progress。本子 PR 按未发布协议直接把 app-channel 当前版本切换到
`proto=2`，并加入窗口注册、窗口关闭和 UI probe result 消息；不保留 proto=1
fallback 或旧错误字段。

## 变更

- shared protocol 的 `PROTO_VERSION` 变为 2，hello error 使用 `current_proto`；
- app → supervisor 增加 `window_registered`、`window_closed`、`ui_probe_result`；
- supervisor 识别这些消息并写入有界事件 journal；
- 模板 runtime handshake 常量同步为 proto 2；
- 当前 app-channel 测试覆盖新握手和窗口/UI 事件路由。

## 可复现验证

    cargo fmt --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs
    git diff --check

窗口实际注册 API、UI 线程 probe 调度和 capture/semantics 仍需后续 runtime PR；本次
只冻结 transport DTO 与 supervisor 事件入口。未发布阶段不提供旧 proto 兼容承诺。
