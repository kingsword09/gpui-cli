# F02：v2 control envelope DTO（2026-09-22）

状态：in_progress。本子 PR 在共享 protocol crate 中冻结设计文档定义的 v2
control 外层结构、错误分支、能力描述和 request_id 约束；不注册 observe/act 命令，
也不改变当前 D1 CLI 行为。

## 契约

- `V2Envelope` 固定 `schema_version=2`、`session_id`、`request_id`、`ok` 和互斥的
  `result|error` 分支；
- `V2Error` 保留稳定 `code`、可读 `message`、可选 `details` 与 `retryable`；
- `Capability` 保留 `available`、可选 `reason`、`provider` 和结构化 constraints；
- request_id 限制为 1–128 个 ASCII 字母/数字/`.`/`-`/`_`，避免把大文本当去重键。

## 可复现验证

    cargo test --locked -p gpui-dev-protocol
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs

DTO 尚未接入 control server、operation 状态机或 runtime；乱序回复、权限校验和 v2
请求路由属于后续 F02/C-05 子 PR。F02 与 T04 继续保持 `in_progress`。
