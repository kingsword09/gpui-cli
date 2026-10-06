# T01 doctor CLI host smoke（2026-10-07）

状态：本地与两套三平台 CI 切片通过，PR #382 已 squash 合并为 `9e6ef64`，PR #383 的
target-selection 扩展已 squash 合并为 `757f05b`；不晋升 T01 或 G0。

## 范围

该切片把 doctor 从 Rust 单元/规则测试推进到真实 CLI 调用。测试覆盖生成项目的显式
target、项目默认 target，以及没有 `gpui.toml` 的 host-only 目录；生成项目路径先执行
`gpui init`，再从项目目录执行：

```text
gpui doctor --json --target desktop
```

断言 schema v2、显式 target/source、overall `pass`、所有 required check 为 `pass`，以及
报告不包含 Android/iOS checks。测试不构建生成项目，不启动窗口，也不自动安装 SDK、许可或
设备；它验证 doctor 的命令边界和 target-aware 选择。

## 本地结果

- 命令：`cargo test --locked --test doctor_cli -- --nocapture`
- macOS arm64：2 passed，exit 0；hosted runner 缺少可选 capture provider 时整体可为 `warning`，但 required checks 必须全部 `pass`。
- 生成项目使用 `--targets macos`，doctor 使用显式 `--target desktop`；required Rust、host
  platform 和 C compiler checks 全部通过，移动工具链未进入报告。

## CI/原生边界

该 integration test 随 PR #382 的两套 `check` workflow 在 Linux/macOS/Windows matrix 中运行
并通过；runs `37496876490`、`37496869916`。PR #383 的 target-selection 扩展也在两套 workflow
中通过；runs `37500590204`、`37500498141`。host smoke 不代替真实 Android ABI、iOS simulator/
physical device 或完整 Linux/Windows 工具版本矩阵。Linux hosted runner 缺少可选 capture provider
时报告 `warning`，required checks 仍全部 `pass`；不修改 CI runner 环境。
