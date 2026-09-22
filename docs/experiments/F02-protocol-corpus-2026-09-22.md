# F02：protocol 输入 corpus 与边界（2026-09-22）

状态：in_progress。本子 PR 补充 C-05 的纯协议输入边界：已知消息允许未知字段，
未知消息和 malformed JSON 拒绝；截断 frame、1 MiB 边界和超限写入均有确定性测试。

## 策略

- 只在读取 4-byte 长度后、分配 body 之前拒绝超过 1 MiB 的 frame；
- 完整长度的 payload 可以恰好达到 1 MiB，读写保持大端长度前缀；
- known message 的未知字段被 serde 忽略，供新旧 runtime 做非破坏性字段扩展；
- unknown `type`、截断头/body 和 malformed JSON 都返回错误，不产生部分消息。

## 可复现验证

    cargo test --locked -p gpui-dev-protocol
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs

未覆盖乱序 request 路由、跨 session 权限和 runtime 产物传输；这些属于后续 C-05/O03
子 PR。F02 与 T04 继续保持 `in_progress`。
