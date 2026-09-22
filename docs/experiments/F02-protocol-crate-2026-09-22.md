# F02：独立 GPUI-dev protocol crate（2026-09-22）

状态：in_progress。本子 PR 建立无 GPUI/window/platform 依赖的
`gpui-dev-protocol` workspace crate，先冻结 v1 framing、消息 enum、可选握手
metadata 和 unsupported-version reply；CLI 与生成 runtime 的接入留给后续 PR，
避免一次改动同时改变 live 状态语义。

## 契约

- 4-byte big-endian length prefix，单帧上限 1 MiB；
- `ClientMessage` / `ServerMessage` 与当前 v1 wire tags 保持一致；
- hello metadata 全部可选，旧 app JSON 可直接 decode；
- `hello_error` 可表达 `unsupported_version` 与 supervisor 支持版本；
- crate 不依赖 GPUI，便于 protocol → runtime → CLI 的发布顺序。

## 可复现验证

    cargo test --locked -p gpui-dev-protocol
    cargo test --workspace --locked

独立 crate 单测覆盖 legacy/current hello、upgrade error、frame roundtrip 和上限。

## 尚未覆盖

CLI/template 改为依赖该 crate、独立 runtime crate、已发布 registry/git package
的兼容矩阵仍需后续 F02 子 PR；F02 继续保持 `in_progress`。
