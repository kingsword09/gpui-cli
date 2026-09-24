# O02：iOS 资源 hash 校验与原子暂存（2026-09-24）

状态：in_review。本子 PR 使用 `assets_begin` 的声明 hash 校验 iOS simulator 收到的
`asset_data`，避免损坏或错 transfer 的字节在 commit 后进入 UI 线程。

## 实现

- runtime 对 base64 解码后的 bytes 计算 SHA-256，并与当前 transfer 的 declared hash 比较；
- hash 缺失或不匹配进入 `assets_received.failed`，错误为 `hash_mismatch`，不创建可读目标；
- 校验通过后先写同目录临时文件，再 rename 到 `gpui-assets/<path>`，避免 UI 看到半写文件；
- 已有 begin/commit 暂存边界保持不变，坏 bytes 即使收到也不会产生成功的 `assets_applied`。

## 边界

本子 PR 的 iOS `asset_data` 校验已与桌面/Android AssetSource 读取校验分开；
`assets_required_loaded` 的声明/回报属于紧随其后的 required-loaded 子 PR。分块 offset/重传
和 `scene_epoch` 也尚未实现。

## 验证

- SHA-256 空串和 `abc` 标准向量；
- 错 hash 的 asset_data 在 commit 后报告失败且不进入成功事件；
- 全新生成 macOS 项目 default/`gpui-dev`/release 编译与 runtime 测试。
