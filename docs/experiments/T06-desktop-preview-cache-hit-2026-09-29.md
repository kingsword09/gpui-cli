# T06：desktop preview verified cache hit（2026-09-29）

状态：`in_progress`。PR #169 已 squash 合并为 `7283614`。本切片把 #165 的 preview output
lock 推进到 desktop preview 的 verified artifact manifest 命中：只有完整验证成功时才跳过
Cargo，其他情况都保守回退普通构建。

## 实现范围

- check 将 target-specific BuildKey hash 随 `GPUI_PREVIEW_BUILD_KEY_HASH` 传入 preview；
  output root 继续来自 source-project 的受控 BuildOutputLayout。
- desktop preview 在成功构建且 build revision 仍 current 后，发布独立的
  `preview-artifact-manifest.json`，不覆盖普通 `artifact-manifest.json`。
- 下一个 desktop preview 在同一 output-root lock 内验证 preview manifest 的 desktop platform、
  BuildKey hash、完整文件集合、size/executable/content hash，并要求 manifest 中只有一个
  `cargo-target/` 可执行文件；验证通过才直接启动该 binary 并跳过 Cargo。
- manifest 缺失、损坏、platform/key 不匹配、文件被修改、新增文件或可执行文件歧义均视为
  cache miss，继续正常 Cargo 构建并刷新 preview manifest。

## 保证与边界

这是 verified output reuse，不是构建所有权协调。当前不提供：

- iOS simulator/真机或 Android preview cache hit；
- 独立 `gpui check`、`gpui build`、`gpui run` 命令之间的共享构建 ownership；
- 同 key 在途任务 coalescing、取消协调或预热队列；
- `build.rs`、Gradle、NDK、Xcode 隐藏输入的自动发现；
- 真实 GUI/设备连续矩阵验收。

普通 Live 未携带 BuildKey/output layout 时继续走原始构建路径；capture-only/mobile lifecycle
也未因此获得 desktop scenario 语义。

## 验证

本地通过：

- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（335 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增测试覆盖 preview manifest 的 platform/key 绑定、内容验证和唯一可执行文件选择；CI
验证见 PR #169，required checks 全部通过。未发布版本，未创建 tag。

## 下一步

复用同一 verified-manifest 语义推进 iOS simulator/Android default-debug preview，再实现
跨独立命令的 build coordinator/ownership；在此之前不把 output lock 或单平台命中扩大为完整
T06/M04 完成。
