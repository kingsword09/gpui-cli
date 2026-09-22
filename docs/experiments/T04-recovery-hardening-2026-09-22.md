# T04：恢复锁与符号链接边界（2026-09-22）

状态：in_progress。本子 PR 补充崩溃后显式 `recover` 的锁清理契约，并把
受管路径的符号链接检查延伸到 plan、apply 和 recovery；不把 T04 标记为
`done`。

## 变更

- `gpui upgrade recover --transaction <id>` 在恢复完成后，只删除内容中记录
  该 transaction id 的 `.gpui/upgrade/upgrade.lock`；锁属于其他 transaction
  时返回 `upgrade_lock_owner_mismatch`，不会覆盖并发升级。
- plan 在读取基线文件时拒绝受管文件或其父目录为 symbolic link。
- apply/recovery 在 hash、backup、replace、delete 和 restore 前后都通过项目
  根相对路径检查；绝对路径、`..` 和 symbolic link 不会被跟随。
- recovery 对路径检查失败会把对应 entry 标为 `recovery_required` 并保留
  用户文件，不把错误当成成功回滚。

## 可复现验证

    cargo test --locked upgrade::
    cargo fmt --check

结果：20 个 upgrade 单测通过，包含锁 owner mismatch、最终文件 symlink、父目录
symlink、计划读取 symlink 和精确 recovery 的既有测试。macOS 临时目录位于
`/var` 时也通过；检查从项目根开始，不会把系统路径别名误判为项目 symlink。

## 未覆盖

本子 PR 尚未增加第二个内嵌 template revision，因此真实 CLI 仍只能执行当前
baseline 的 no-op apply；replace/add/delete 的真实旧模板迁移、manifest 阶段
crash、native/toolchain validation 及完整 T-06/T-07 矩阵继续由后续 T04 子 PR
覆盖。
