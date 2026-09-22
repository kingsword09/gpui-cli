# F02：当前 control 协议直接切换到 envelope/request_id（2026-09-22）

状态：in_progress。本子 PR 按未发布产品处理，直接把 supervisor control API 切换到
当前 `schema_version=2` envelope；不保留旧 Reply 结构、旧 supported-schema 列表或
旧请求适配层。

## 变更

- 每个 control request 必须携带受限 `request_id`；回复必须回显同一个 ID；
- 成功/失败回复统一使用共享 `V2Envelope<Value>`；
- session registration 只声明当前 schema 2；
- CLI status/diagnostics/events 复用同一请求 ID，并将结构化错误映射回本地错误类型；
- token/session/schema/request_id 都在 supervisor 侧校验，乱序或错身份回复不被接受。

## 可复现验证

    cargo fmt --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs
    git diff --check

这一步只覆盖 control transport 和请求身份；app-channel、runtime 窗口消息和 observe/act
仍在后续直接破坏性改动中实现。未发布阶段不提供旧 schema 兼容承诺。
