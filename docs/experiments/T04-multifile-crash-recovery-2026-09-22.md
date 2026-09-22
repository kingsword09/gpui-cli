# T04：真实多文件 crash/recover 矩阵（2026-09-22）

状态：in_progress。本子 PR 将跨进程 crash 证据从同版本 no-op apply 扩展到
`agent-native-v0-legacy` → `agent-native-v1-draft` 的真实 replace/add/delete 加
manifest 事务。

## 覆盖点

测试在每个点终止独立 apply 进程，随后由另一个 `upgrade recover` 进程恢复：

- `after_journal`
- `after_backup`
- `before_replace` / `after_replace`
- `before_manifest` / `after_manifest`
- `before_validate` / `after_validate`

每轮都检查 journal 在 crash 时不是 `committed` 或 `rolled_back`，recover 最终
报告 `rolled_back`，锁被移除，并逐字节比较 `lib.rs`、新增/删除 runtime 文件和
template manifest，确认项目回到 crash 前的 legacy baseline。

## 可复现验证

    cargo test --locked --test upgrade_crash_recovery -- --nocapture

这条测试同时证明真实 migration plan 的 base/target 是 v0/v1；不是用同版本
no-op 事务伪造多文件恢复证据。

## 尚未覆盖

磁盘满/权限错误的跨平台注入、用户在多文件事务中途编辑后的完整 CLI 证据，以及
native build/package 仍需后续 T04 子 PR；T04 继续保持 `in_progress`。
