# S04：preview 构建输出跨进程锁（2026-09-29）

状态：`in_progress`。PR #165 已 squash 合并为 `cca46b3`。本切片把 #163 的
target-specific source-project output layout 从“路径隔离”推进到“进入构建前取得同一输出根的
跨进程排他锁”；不声称 cache hit、manifest 复用、跨命令构建所有权或在途任务 coalescing。

## 实现范围

- `BuildOutputLock::acquire_at_root` 在已规划的 output root 下创建/打开持久 lock file，并使用
  OS 文件锁；锁文件保留在磁盘上，进程退出或崩溃时由操作系统释放锁。
- desktop preview 的 Cargo 构建、iOS simulator/physical 构建和 Android preview 构建均在
  source-project 的 target-specific output root 上取得 guard，覆盖当前 live build 过程。
- check 将 `BuildOutputLayout.root` 作为 `GPUI_PREVIEW_BUILD_OUTPUT_ROOT` 传给 preview；
  preview 同时继续使用已有的 Cargo target、iOS DerivedData、Android JNI/Gradle 子目录布局。
- 保留原有 `BuildOutputLock::acquire(layout)` 的 prepare-before-use 语义；preview 的
  `acquire_at_root` 只负责锁定已规划的 root，不重复准备或清理 output layout。

## 锁的保证

同一 source-project、platform/ABI、BuildKey 对应的 output root，只要构建路径使用该 guard，
多个独立 preview 进程不会同时修改该 root 下的 Cargo/JNI/Gradle/DerivedData 输出。锁 guard
释放后另一个等待者可以继续；lock file 本身不包含构建状态，也不代表输出已经成功、完整或可复用。

因此本切片仍明确不提供：

- 已完成 artifact manifest 的 cache hit 或输出复用判定；
- 独立 `gpui check`/`gpui build` 命令之间的共享构建所有权；
- 同 key 在途任务合并、调用者取消协调或预热队列；
- `build.rs`、Gradle、NDK、Xcode 隐藏输入的自动发现；
- 真实 macOS 窗口、iOS simulator/真机、Android 设备连续矩阵验收。

## 验证

本地通过：

- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（332 个单元测试及全部集成/协议测试）
- `cargo fmt --all -- --check`
- `cargo x check-design-docs`
- `git diff --check`

新增 `preview_root_lock_is_released_after_the_guard_drops` 测试：第一个 guard 持有 root
锁时，另一个线程不能取得同一 root；释放第一个 guard 后，等待者取得锁并完成，最后 root
仍可重新加锁。该测试验证阻塞/释放语义，不替代真实三端构建竞争验收。

PR #165 的 required CI 全部通过：三 OS check、desktop-template、android-template、
baseline-driver 均通过。未发布版本，未创建 tag。

## 下一步

继续在该锁和 BuildKey/output layout 证据上实现独立命令间的 build coordinator/ownership，
并在能验证完整 artifact manifest 后再定义 cache hit/coalescing 语义；同时补齐 matrix steps/
cleanup 报告以及真实 macOS/iOS/Android 连续验收。
