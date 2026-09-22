# T04：真实进程 crash/recover（2026-09-22）

状态：in_progress。本子 PR 用隐藏测试环境变量让 apply 进程以退出码 75
终止，验证锁的 Drop 不会运行时仍能由独立 `upgrade recover` 进程恢复。

## 覆盖范围

- `after_journal`：journal 已落盘但 apply 进程退出。
- `after_backup`：备份阶段 checkpoint 完成且 journal 状态已写入后退出。
- `before_validate`、`after_validate`：写入/验证阶段留下未完成 journal 后退出。
- 每个点都从锁文件读取 transaction id，检查 journal 不是 committed/rolled_back，
  再运行独立 recover CLI，最终得到 `rolled_back` 并删除 lock。

测试钩子：`GPUI_UPGRADE_CRASH_POINT=<point>`，仅用于测试进程终止，不是公开
用户配置；退出码 75 代表 injected crash。

## 可复现验证

    cargo test --locked --test upgrade_crash_recovery
    cargo fmt --check

## 尚未覆盖

当前 CLI 仍只有一个 embedded template revision，因此 replace/manifest 的真实
多文件 crash 需要第二个 target revision；磁盘满、权限错误和完整 T-06/T-07
证据仍需后续 T04 子 PR。
