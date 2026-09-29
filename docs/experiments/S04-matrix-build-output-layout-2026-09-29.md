# S04：matrix preview 绑定 target-specific 输出布局（2026-09-29）

状态：`in_progress`。PR #163 已 squash 合并为 `4f35c95`。本切片把 #161 的 target-specific
BuildKey 从报告证据推进到 preview build 的 source-project 输出路径；不声称跨命令共享构建或
在途任务合并已经完成。

## 实现范围

- matrix 为每个 ready target 从 shared frozen root 计算 BuildKey，并在源项目
  `.gpui/builds/<platform>/<key>/` 下准备 `BuildOutputLayout`。
- desktop preview 使用 key-scoped Cargo target dir；iOS simulator 使用 key-scoped Cargo
  target 与 DerivedData；Android 使用 key/ABI-scoped Cargo target、JNI staging 和 Gradle
  build dir。
- check 将这些绝对输出路径通过受控 preview 环境传入子进程；preview 的 live builder 继续
  从 frozen runtime root 读取源码，但把编译/打包产物写到 source-project 的布局。
- 同一 matrix target BuildKey 的 cells 增加 `build:<sha256>` resource id，在当前 matrix
  supervisor 内串行，避免多个 scenario 同时写同一 Cargo/JNI/Gradle/DerivedData 目录。
- Android matrix ABI 同时传给 preview build environment，避免实际 ABI 与 context BuildKey
  维度分离。

## 验证

PR #163 合并前已通过：

- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（331 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增测试验证 BuildKey layout 位于 canonical source-project `.gpui/builds/desktop` 输出根。
PR #163 的 required CI 全部通过；Windows pointer-dispatch 既有测试首次超时后重跑通过。该
PR 未发布版本或 tag。

## 未覆盖与边界

- resource id 只约束同一个 matrix supervisor；不同 `gpui check`/`gpui build` 进程之间仍无
  共享 BuildOutputLock、在途任务所有权或 build coalescing。
- 尚未基于 verified artifact manifest 做 matrix preview cache hit/复用判定，也没有取消安全的
  shared build coordinator；key-scoped 路径不等于构建已完成或已复用。
- `build.rs`、Gradle、NDK、Xcode 隐藏输入仍受严格 snapshot 的保守拒绝边界约束；真实窗口、
  simulator/emulator、viewport/DPI 和完整 steps/cleanup evidence 仍未验收。
