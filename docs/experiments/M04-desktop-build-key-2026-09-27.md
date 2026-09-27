# M04：desktop BuildKey 接入切片（2026-09-27）

状态：`in_progress`。本切片把 desktop `build`/`run` 接到真实 BuildKey，并将 Cargo
target 放入按 `(platform, BuildKey)` 隔离的输出目录；不宣称移动端或完整冻结快照构建已经
接入。

## 行为

- `desktop_build_key` 使用稳定源码 manifest、`Cargo.lock`、`NativeInputs`、`rustc -vV`
  的 host/toolchain 摘要、desktop target/profile 和显式环境 allowlist；环境 allowlist
  只包含 `CARGO_BUILD_TARGET`、增量/profile 选项、`CC`/`CXX`、deployment target、
  `RUSTC_WRAPPER` 和 `RUSTFLAGS`；
- `features`、ABI 和 preview registry 维度仍按 desktop 当前调用边界显式取空/`none`，
  不从未声明来源猜测输入；release 与 dev profile 生成不同 BuildKey；
- `gpui build desktop` 与 `gpui run desktop` 在执行 Cargo 前创建
  `.gpui/builds/desktop/<key>/cargo-target`，并通过 `CARGO_TARGET_DIR` 使用该目录；
- 相同输入和 profile 重复得到相同路径，不同源码/native 输入、工具链摘要、allowlist
  值或 profile 会得到不同路径；布局不会清理其他 key 的输出。

## 验证

- `runner::build_inputs` 单元测试验证 dev/release key 不同、desktop layout 位于
  `.gpui/builds/desktop/<key>`、缺失 `Cargo.lock` 在 key 中显式记录为 `missing`；
- BuildKey 测试验证所有维度变化会改变摘要，环境 allowlist 排序稳定且重复名称拒绝；
- 本 PR 门槛结果：`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo test --workspace --locked`（220 个 unit tests 及全部 integration/doc tests）和
  `cargo x check-design-docs` 均通过；设计文档检查报告 80 个 Markdown 文件、111 个
  本地链接、35 个任务和 68 个验收用例。

## 未覆盖

- desktop 非 live 命令已由后续 [desktop frozen build root](M04-desktop-frozen-build-root-2026-09-27.md)
  切片改为从临时 `FrozenInputs` 副本构建；本文件仍只记录最初的 BuildKey/输出接入，
  不重复声明快照编排已完成；
- Android JNI、iOS DerivedData、移动 ABI/features、snapshot build orchestration 和
  同 key 在途任务合并尚未接入；
- `build.rs`、Gradle、Xcode 可能读取的未声明文件仍不在自动发现范围内，不能据此宣称
  完整输入闭包或跨平台可复现构建。
