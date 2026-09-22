# F02：runtime/GPUI 版本与 capabilities 握手元数据（2026-09-22）

状态：in_progress。本子 PR 在保持 v1 app-channel framing 和消息语义不变的前提
下，为新生成 runtime 增加可选握手元数据；旧 v1 客户端缺少这些字段时仍能被
server 接受。

## 元数据

- `runtime_version=agent-native-dev-runtime-v1`；
- `gpui_version` 来自模板锁定的 `gpui-pre` 版本；
- `capabilities` 当前报告 `logs`、`panic`、`state`，以及启用 asset source 时的
  `asset_reload`；
- server 将值裁剪到有界长度/数量后写入 `app.connected` 事件；不把客户端声明的
  build/run 身份当作可信来源，身份仍由 token 和 supervisor scope 绑定。

## 兼容性

旧 hello JSON 没有 runtime metadata 时使用空默认值；因此这是 v1 的可选字段扩展，
不会改变旧 CLI/app 的 follow、state、asset 和 panic 路径。与上一子 PR 的未知
proto `hello_error` 组合后，认证、版本和能力三类结果仍可区分。

## 可复现验证

    cargo test --locked devserver::protocol::tests::hello_metadata_is_optional_for_legacy_clients
    cargo test --workspace --locked

生成模板的 desktop-template CI 会继续编译渲染后的 debug/release/feature 组合，
确保 placeholder 替换后的 hello 代码可编译。

## 尚未覆盖

真正独立的 `gpui-dev-protocol` / `gpui-dev-runtime` crate、runtime semver 协商、
v2 observation capabilities 和旧 supervisor/archive reader 矩阵仍需后续 F02 子 PR；
F02 继续保持 `in_progress`。
