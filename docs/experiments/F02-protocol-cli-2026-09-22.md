# F02：CLI 接入共享 GPUI-dev protocol crate（2026-09-22）

状态：in_progress。本子 PR 将 CLI app-channel 的消息、JSON 编解码和有界
length-prefixed framing 切换到已合并的 `gpui-dev-protocol`，保留 CLI 本地模块的
错误上下文和 base64 资产辅助函数。

## 保留的兼容契约

- CLI 继续通过 `src/devserver::protocol` 使用同一组导出名称；控制通道和 live
  逻辑不需要改变调用方式。
- v1 的 JSON `type` 标签、4-byte big-endian frame 和 1 MiB frame 上限由共享 crate
  实际提供。
- 没有 runtime/gpui/capabilities 字段的旧 hello 仍可解码；当前 hello 的可选元数据
  直接进入 `AppConnected` 事件，仍按既有长度上限裁剪。
- 模板 app 本身本次不改，继续以现有手写 v1 JSON 发送端作为旧/当前客户端夹具。

## 可复现验证

    cargo fmt --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    cargo x check-design-docs
    git diff --check

## 尚未覆盖

模板切换到已发布或不可变 Git revision 的 protocol 依赖、独立 runtime crate、旧
CLI/模板/新 runtime 的三代兼容矩阵仍需后续 F02 子 PR；F02 继续保持 `in_progress`。
