# T01 doctor CLI host smoke（2026-10-07）

状态：本地切片通过，等待新 PR 的三平台 CI/review；不晋升 T01 或 G0。

## 范围

该切片把 doctor 从 Rust 单元/规则测试推进到真实 CLI 调用。测试在临时目录执行
`gpui init` 生成 desktop-only 项目，再从项目目录执行：

```text
gpui doctor --json --target desktop
```

断言 schema v2、显式 target/source、overall `pass`、所有 required check 为 `pass`，以及
报告不包含 Android/iOS checks。测试不构建生成项目，不启动窗口，也不自动安装 SDK、许可或
设备；它验证 doctor 的命令边界和 target-aware 选择。

## 本地结果

- 命令：`cargo test --locked --test doctor_cli -- --nocapture`
- macOS arm64：1 passed，exit 0。
- 生成项目使用 `--targets macos`，doctor 使用显式 `--target desktop`；required Rust、host
  platform 和 C compiler checks 全部通过，移动工具链未进入报告。

## CI/原生边界

该 integration test 会随现有 `check` job 在 Linux/macOS/Windows matrix 中运行。新 PR 尚未
创建，三平台结果待 CI 提供；host smoke 也不代替真实 Android ABI、iOS simulator/physical
device 或完整 Linux/Windows 工具版本矩阵。若 hosted runner 的必需工具缺失，保留 doctor 的
required failure/unavailable 结果并记录恢复条件，不修改 CI runner 环境。
