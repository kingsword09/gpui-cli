# M04：desktop frozen build root 接入切片（2026-09-27）

状态：`in_progress`。本切片把非 live 的 desktop `build`/`run` 从“只用稳定扫描生成 key”
推进到“先创建稳定冻结副本，再从副本执行 Cargo”。外部 Cargo path package 会复制到
快照并重定位；iOS/Android、live builder 和 `build.rs` 隐藏输入不在本切片范围内。

## 行为

- desktop build plan 先执行锁定的 Cargo metadata scope，再用 `Inputs::freeze_to_with_cargo_scope`
  创建临时 `FrozenBuildRoot`；副本包含 workspace 与允许的外部 path package，副本内 Cargo
  manifest 的 path 只指向 relocated `external/NNNN`；
- BuildKey 的 `source_manifest_hash` 使用 `FrozenInputs.input_hash`，因此外部 path package
  内容变化会改变 key，而不是仅依赖 workspace 内 manifest；native input digest 也从冻结
  root 采集；
- `gpui build/run desktop` 将 Cargo `current_dir` 指向临时冻结 root，并将
  `CARGO_TARGET_DIR` 指向源项目 `.gpui/builds/desktop/<key>/cargo-target`；临时副本在
  命令完成后自动清理，源工作区不会被改写；
- 输出目录仍按源项目的 key 布局保存，便于重复执行时保持输出隔离；快照生命周期只覆盖
  一次命令，尚未实现持久 snapshot cache 或同 key 任务合并。

## 验证

- 单元测试验证 desktop build plan 的 Cargo root 是冻结副本、快照含 Cargo workspace、
  BuildKey source hash 使用 frozen input hash；已有外部 package relocation 测试继续覆盖
  源 manifest 不修改；
- 本 PR 门槛结果：`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo test --workspace --locked`（223 个 unit tests 及全部 integration/doc tests）和
  `cargo x check-design-docs` 均通过；设计文档检查报告 83 个 Markdown 文件、116 个
  本地链接、35 个任务和 68 个验收用例。真实 desktop 应用运行仍按平台和项目工具链
  验收，不把本地无 GUI 结果扩大为跨平台结论。

## 未覆盖

- iOS/Android 非 live build 仍使用工作目录作为 native/Xcode/Gradle 工作根；
- live builder、持久 snapshot cache、snapshot build orchestration、同 key 在途任务合并
  尚未接入；
- `build.rs`、Gradle、Xcode 的未声明隐藏 I/O 仍不在自动发现范围内，冻结 manifest 不能
  被表述为完整构建输入闭包。
