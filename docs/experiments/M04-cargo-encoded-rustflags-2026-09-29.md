# M04：Cargo encoded Rust flags 缓存键修复（2026-09-29）

状态：`limited_adopt`。该记录只覆盖 BuildKey 环境 allowlist 的一个已确认遗漏，
不代表 M04/T06 整体完成。

## 问题

审计探针在修复前发现：构建参数经 Cargo 编码到 `CARGO_ENCODED_RUSTFLAGS` 后，
BuildKey 只读取 `RUSTFLAGS`，因此不同编译参数可能复用同一个 artifact cache 条目。

## 实现

- PR #149（合并提交 `1256bb3`）把 `CARGO_ENCODED_RUSTFLAGS` 加入
  `src/runner/build_inputs.rs` 的显式环境 allowlist。
- 新增 `encoded_rustflags_change_the_build_environment_hash` 回归测试；测试使用
  Cargo 的 unit-separator 编码形式构造两个 allowlist 输入，不修改共享进程环境。

## 验证

- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`：359 passed，0 failed
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`
- PR CI：Linux/macOS/Windows check、desktop-template、android-template、baseline-driver 全部通过。

## 边界

本切片证明该 allowlist 变化会改变 BuildKey 环境 hash，但没有把旧的 CLI cache probe
结果改写成修复后的端到端证据。build.rs/Gradle/NDK/Xcode 隐藏输入、check/matrix
冻结执行器、同 key 在途任务共享和完整 T-10/M-08 验收仍未完成。
