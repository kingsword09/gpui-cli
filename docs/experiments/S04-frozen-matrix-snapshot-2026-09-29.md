# S04：matrix 共享冻结 workspace snapshot（2026-09-29）

状态：`in_progress`。PR #157 已 squash 合并为 `97406bb`。本切片把 matrix 的输入边界从
“仅 admission 声明 `source_mode = frozen`”推进到“admission 前实际创建并重核验一个 snapshot，
所有 cell 复用它”；target-specific BuildKey、完整报告传播和真实三端验收仍未完成。

## 实现范围

- `gpui check --matrix <file>` 先确保 `Cargo.lock` 存在，再创建一个严格 `FrozenCheckInputs`
  workspace snapshot；已知 local `build.rs` 隐藏输入会直接返回不可用，不回退到 source root。
- scenario file 和 matrix file 必须位于项目 root 内，并从 snapshot 中重新读取；scenario
  validation、TOML parsing 和 matrix admission 都绑定 snapshot root。
- matrix admission 完成后，factory 将同一个 snapshot root、snapshot hash 和 snapshot 内的
  scenario path 传给每个 cell runner；不同 target/scenario 不再各自从可变项目 root 启动。
- preview 子进程以共享 snapshot root 为 current directory；原项目 root 仍用于 check 报告的
  baseline/diff 以及移动 native artifact/lease 路径。
- `CheckLaunchOptions` 为每个 matrix runner 保留 shared snapshot hash；本切片不伪造
  target-specific BuildKey，MatrixReport 仍沿用现有 status/error/capture artifact 投影。

## 验证

PR #157 合并前已通过：

- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（330 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增单元测试验证 scenario/matrix 两个输入都位于同一个非 source workspace snapshot；PR #157
的三 OS check、两组 desktop-template、两组 android-template 和 baseline-driver required CI
全部通过。该 PR 未发布版本或 tag。

## 未覆盖与边界

- preview 内部仍根据 target 执行各自的构建路径；本切片只保证它们从同一 frozen input root
  启动，不提供 target-specific BuildKey、共享构建产物或同 key 在途任务合并。
- local `build.rs`、Gradle、NDK、Xcode 等未建模隐藏 I/O 仍不在通用输入闭包内；已知 local
  `build.rs` 路径被保守拒绝，不能把 snapshot hash 写成完整输入闭包证明。
- `MatrixCellExecution`/`MatrixReport` 仍只保留 status、primary error 和 capture artifact
  IDs；单场景 `CheckReport.context` 的 reset generation、environment、snapshot hash 等尚未
  传播到 matrix JSON。
- 没有把纯 Rust/无设备 CI 写成 macOS 窗口、iOS simulator、Android emulator 的真实矩阵
  验收；移动 semantics/input、viewport/DPI 和日志归属仍按现有 capability 边界处理。
