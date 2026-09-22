# F02：dev-channel 协议版本边界（2026-09-22）

状态：in_progress。本子 PR 补充 C-01 的第一段握手契约：认证成功但协议版本不
支持时返回结构化升级错误；认证失败仍静默关闭连接，避免泄露 supervisor 能力。

## 行为

- 当前协议版本仍为 v1，现有 `hello_ok`、日志、panic、state、asset 消息不变；
- 合法 token + 未知协议版本返回 `hello_error`：
  `code=unsupported_version`、提示文本和 `supported_proto`；
- 错误 token 即使携带未知版本也不返回版本信息；
- 旧生成 app 只接受 `hello_ok`，收到 `hello_error` 后断开并按原有重试路径等待，
  不把兼容错误当成认证成功或运行状态。

## 可复现验证

    cargo test --locked devserver::app_channel::tests::valid_token_with_unknown_protocol_gets_an_upgrade_error
    cargo test --locked devserver::protocol::tests::unsupported_protocol_replies_are_structured

测试同时保留错误 token 静默拒绝和正确 v1 握手成功断言。

## 尚未覆盖

runtime_version/capabilities 字段、v2 observation API、旧 supervisor/archive reader
的真实二进制兼容矩阵仍需后续 F02 子 PR；F02 继续保持 `in_progress`。
