# T01 doctor 本机验收摘要（2026-10-06）

状态：PR #380 的 macOS arm64 本地责任子例已执行；摘要供代码审查使用。原始命令输出与图片保存在本机忽略目录 `artifacts/acceptance/1c0a4cb/`，不属于 GitHub checkout；本页只记录可审查结论，不冒充 CI 或完整设备矩阵。

## 环境

- 执行分支：`codex/t01-doctor-closeout`；提交：`c278dbc`；基线 `origin/main`：`1c0a4cb`。
- macOS 15.6.1 (24G90)、arm64；Rust 1.97.1、Xcode 26.2、Java 21.0.8。
- Android SDK platform 34、build-tools 36.0.0、NDK 27.2.12479018、Gradle wrapper 9.4.1、AGP 9.1.0。
- 未记录环境秘密；未安装 SDK、接受许可或修改签名。

## T-01 · 按目标选择必需工具

在生成的 desktop-only、iOS-only、Android-only 项目运行 doctor。三个项目均退出 0；desktop-only 隐藏 Android/Xcode 命令时仍退出 0，并只对可选 capture 工具给 warning。另测非项目显式 target、项目默认 target 和显式设备选择。Android 项目报告实际选定的 SDK/JDK/NDK/Gradle/AGP；错误 compileSdk、缺 build-tools、缺 NDK `source.properties`、Java 11 对固定模板 AGP/Gradle 组合均按 required failure 处理。动态 compileSdk 和未建模 AGP/Gradle 组合保持 Unknown。

临时 SDK secret canary 未出现在 JSON 或 stderr。探测报告只输出环境变量来源/存在性，不输出其值。

## T-02 · 命令退出与版本语义

受控子进程回归覆盖 exit 0 + 畸形版本、有效版本、stderr 版本、无版本的非版本命令、非零退出、100ms timeout、128 KiB stderr 截断、Rust minimum mismatch、Gradle wrapper match/mismatch。定向 toolchain 测试为 35 passed、10 ignored；`xcodebuild -version` 实际工具对照解析为 Xcode 26.2。

## T-03 · SDK/JDK/ABI、候选设备与秘密

- 22 台可用 iOS simulator 中，名称+runtime 和 UDID 两个 selector 分别解析到不同设备；均未启动。
- 本机发现 3 台 stopped Android AVD；`Pixel_9_Pro` 和 `Pixel_9a` selector 分别解析正确，调用前后仍为 stopped。
- ARM64 AVD 配默认 `arm64-v8a` 构建 ABI 返回 pass；同一设备将 `GPUI_ANDROID_ABIS=x86_64` 时 required device check 返回 fail，expected/actual 和 mismatch 原因均在报告中。缺失 ABI 的 Unknown 与 required Rust target 缺失由定向单测覆盖。
- `adb devices -l` 无连接的 Android 设备；physical Android、真实 x86 AVD 与 Linux/Windows 变体未运行。

## 本地回归与 PR CI

- `cargo test --workspace --locked -- --test-threads=1`：主二进制 488 passed、11 ignored，其他 32 个测试目标通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo build --locked`、`cargo fmt --check`、`git diff --check`、`cargo x check-design-docs`、baseline-driver self-test 和 Python compile 均通过。
- 首次默认并行 workspace run 中 3 个既有 process-tree cleanup 测试超时；三个测试分别重跑通过，失败尝试单独保留，不覆盖。
- PR #380 首轮 macOS `cargo test` 和 clippy 通过，但设计文档检查因链接到被 `.gitignore` 排除的本地 artifact 目录而失败。该引用现改为本页；首轮 Linux/Windows job 因 workflow fail-fast 被取消，必须在新提交上重跑。
- 本地 workspace/selector 结果不等同于跨平台 CI、physical-device 或 F01 GUI 验收。

## 本机原始证据位置

原始 JSON、命令和子进程探针分别保存在 `artifacts/acceptance/1c0a4cb/T-01/macos-arm64/attempt-03/`、`T-02/macos-arm64/attempt-02/` 与 `T-03/macos-arm64/attempt-01/` 至 `attempt-06/`。这些路径只在本机存在；本页摘要和 PR 检查是仓库内可审查材料。

## Ubuntu `cc --version` 回归与 CI 复查

PR #380 head `739d140` 的 CI 检查了真实 Ubuntu 24.04 runner 输出
`cc (Ubuntu 13.3.0-6ubuntu2~24.04) 13.3.0`。doctor 将 `desktop.c_compiler` 判为失败，导致
`upgrade::apply::tests::clean_plan_commits_noop_transaction_and_releases_lock` 失败。两次触发分别为
[run 37433935925](https://github.com/kingsword09/gpui-cli/actions/runs/37433935925) 和
[run 37433939343](https://github.com/kingsword09/gpui-cli/actions/runs/37433939343)。同一 head 上，一次
macOS complete check 通过；另一次 macOS clippy 因 runner 无法解析 `index.crates.io` 失败。Windows
job 被 fail-fast 取消，没有 Windows 失败结论。模板与 baseline-driver jobs 均通过。

本地工作区在 `739d140` 上为版本 marker 增加 `cc `，并新增 Ubuntu 输出 shim 回归。验证结果：

- `cargo test --locked --bin gpui toolchain::probe::tests::cc_version_with_distribution_prefix_is_parseable`：1 passed。
- `cargo test --locked --bin gpui upgrade::apply::tests::clean_plan_commits_noop_transaction_and_releases_lock`：1 passed。
- `cargo test --workspace --locked`：主二进制 489 passed、12 ignored；所有 integration、protocol、xtask 和 doc-test targets 通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo build --locked`、`cargo fmt --check` 和 `git diff --check` 通过。

这组结果来自含未提交 parser 修复的 macOS arm64 工作区；不是 CI，也不能替代 Linux/Windows doctor 验收。修复推送后需要绑定新 head 的完整 CI 结果。
