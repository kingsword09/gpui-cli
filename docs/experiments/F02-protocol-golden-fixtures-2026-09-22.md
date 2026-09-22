# F02：v1 protocol golden fixtures 与发布元数据（2026-09-22）

状态：in_progress。本子 PR 为独立 `gpui-dev-protocol` crate 增加 legacy/current
hello、unsupported-version reply 和 bounded frame 的 wire golden fixtures，并让
可选 hello metadata 在缺省时不改变旧客户端的 JSON 形状。

## 契约

- legacy hello 重新编码时省略 `runtime_version`、`gpui_version` 和空
  `capabilities`；
- current hello 保留 runtime/gpui/capabilities 字段及既有 snake_case tags；
- structured `hello_error` 和 4-byte big-endian frame 前缀有精确 fixture 断言；
- crate manifest 补充 repository/readme/keywords/categories，`cargo publish --dry-run`
  可完成打包与验证。

## 可复现验证

    cargo test --locked -p gpui-dev-protocol
    cargo publish --dry-run --locked --allow-dirty -p gpui-dev-protocol
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs

实际 crates.io 发布不在本子 PR 范围内；模板引用 protocol 的 release/package 接入须
在 crate 发布或获得明确发布授权后单独推进。F02 与 T04 继续保持 `in_progress`。
