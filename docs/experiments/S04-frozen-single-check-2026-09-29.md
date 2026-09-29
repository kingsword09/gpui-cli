# S04：单场景 desktop check 冻结输入接线（2026-09-29）

状态：`in_progress`。PR #155 已 squash 合并为 `12b1338`。本切片把单场景 desktop check
接入已有的冻结 workspace/build-plan 能力；matrix check、真实 GUI 连续验收和完整环境控制
仍不在范围内。

## 实现范围

- `gpui check --scenario <id> --target desktop` 先调用 `desktop_build_plan`，创建并重核验
  `FrozenInputs` workspace snapshot，并使用 snapshot 的 input hash 和 BuildKey。
- scenario 文件必须位于项目 root 内；命令从 snapshot 中重新读取、解析和静态校验 scenario
  与 fixture，不继续使用可变 source root 中的已解析对象。
- snapshot 若缺少 scenario，或冻结后的校验/解析失败，check 在启动 preview 前失败。
- local `build.rs` 等使 BuildKey cache reuse 不安全的已知输入会让严格 frozen check 直接
  返回不可用；不会退回可变项目目录执行。
- preview 子进程以 snapshot root 为 current directory；原项目 root 仅保留给 baseline/diff
  输出，因此报告仍能写回 source project 的 `.gpui/checks/`。
- 单场景 `CheckReport.context` 记录 `snapshot_hash` 与 `build_key`。matrix runner 继续
  使用原有路径并显式不填这两个字段，本切片不声称 matrix 已冻结。

## 验证

PR #155 合并前已通过以下本地门槛：

- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（329 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

PR #155 的 required CI 也全部通过：三 OS `check`、两组 `desktop-template`、两组
`android-template` 和两组 `baseline-driver` 均成功。该 PR 未发布版本或 tag。

## 未覆盖与边界

- matrix targets/scenarios 尚未共享一个 snapshot；`source_mode = "frozen"` 的 admission
  不能代替实际创建和消费冻结输入。
- `build.rs`、Gradle、NDK、Xcode 等未建模隐藏 I/O 仍不在通用输入闭包内；已知 local
  `build.rs` 路径会保守禁用严格 desktop check。
- 没有把无 GUI 环境下的编译/协议测试写成真实窗口、viewport、DPI、字体/backend 或
  accessibility 验收；移动语义和输入仍由 M01/M02/S03 的后续验收负责。
- matrix 汇总仍只保留现有的 status、primary error 和 capture artifact 投影，尚未传播
  单场景 context、完整 steps 或 cleanup 详情。
