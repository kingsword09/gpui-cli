# S04：desktop preview BuildKey coordinator（2026-09-30）

状态：PR #179 已 squash 合并为 `a9e008c`。本切片把 desktop live preview/check 构建接入
BuildKey coordinator；普通 `build`/`run` 与 preview 各自维护 attempt 状态，但对同一个 Cargo
output root 仍由共享 OS 输出锁串行。iOS/Android preview 尚未接入 coordinator，本记录不代表完整
三端 preview orchestration 已完成。

## 实现范围

- desktop preview leader 只执行一次 `cargo build`，并在构建后发布绑定 platform/BuildKey、逐文件
  校验的 `preview-artifact-manifest.json`；已验证的唯一 desktop executable 可直接复用。
- follower 等待当前 preview attempt，并在接受 succeeded 前重新验证 preview manifest；无效或缺失
  manifest 不作为成功结果。
- 普通 build/run 与 preview 使用不同 coordinator `kind`，并分别写入
  `.build-coordinator.json` 和 `.preview-build-coordinator.json`。两类 attempt 不能互相加入或覆盖
  对方的终态记录；实际 Cargo 输出仍共用 `.build-output.lock`，避免共享 target 目录并发写入。
- preview 构建失败映射为 `BuildFailed`；构建期间输入变化映射为 `Superseded`。未启用安全 cache
  reuse 的输入继续走原有 output lock/构建路径。
- iOS/Android preview 保留既有 output lock 与 verified preview manifest 路径，后续分别接入 coordinator。

## 验证

本地通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`（349 个单元测试及全部集成/协议测试）
- `cargo x check-design-docs`
- `git diff --check`

回归覆盖 preview/build attempt 隔离、对应状态文件保留、跨线程/跨进程同 key 协调、失败共享与
leader 消失接管。Windows CI 首次运行暴露了既有失败共享测试依赖固定睡眠的时序假设；测试现改为
等待 follower subscriber 文件实际出现后才释放失败 leader。之后 PR 与 push 两组 Windows、macOS、
Linux、desktop-template、android-template 和 baseline-driver 检查均通过。

## 未覆盖与下一步

本切片没有接入 iOS 或 Android preview coordinator，也没有加入调用者取消引用、heartbeat/fencing、
queued/cancelled/partial 状态或隐藏构建输入发现。接下来先接入 iOS preview，再接入 Android
preview；每项分别验证平台专用 manifest 和并发等待行为。真实设备/桌面连续矩阵验收仍需另行完成。
未发布版本，未创建 tag。
