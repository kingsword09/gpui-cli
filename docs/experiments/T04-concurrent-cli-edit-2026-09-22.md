# T04：跨进程并发编辑保护（2026-09-22）

状态：in_progress。本子 PR 将 T-07 的“事务写完一个文件后用户编辑该文件”和
“同时启动第二次升级”从纯文件测试扩展为真实 CLI 进程证据。

## 场景

1. 通过 v0→v1 migration plan 启动第一个 `upgrade apply`。
2. 第一个进程写完 `crates/app/src/lib.rs` 后在隐藏测试 gate 暂停。
3. 第二个独立 `upgrade apply` 使用同一个 plan，返回 `upgrade_busy`，不创建第二
   个 transaction。
4. 外部用户追加编辑已写入的 `lib.rs`，放行第一个进程。
5. apply 的最终写入 hash 检查发现 `concurrent_edit`，精确 recovery 恢复其余
   文件，但保留用户编辑；随后独立 `upgrade recover` 报告
   `recovery_required` 和保留路径。

## 可复现验证

    cargo test --locked --test upgrade_concurrent_edit -- --nocapture

断言包括：

- `upgrade_busy` 阻止第二事务；
- 用户编辑后的 `lib.rs` 字节不被覆盖；
- `agent_runtime.rs` 被删除、`legacy_runtime.rs` 和 legacy manifest 恢复；
- apply 失败信息包含 `concurrent_edit` 与 `preserved_user_changes`；
- recover JSON 保留 `crates/app/src/lib.rs`，锁最终不存在。

## 尚未覆盖

磁盘满/权限错误的跨平台注入、native build/package 和完整 T-06/T-07 其他平台
矩阵仍需后续 T04 子 PR；T04 继续保持 `in_progress`。
