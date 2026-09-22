# T04：StorageFull / PermissionDenied 恢复（2026-09-22）

状态：in_progress。本子 PR 为事务底层写入增加一次性、仅由隐藏测试环境变量
触发的 I/O 错误注入，验证真实 v0→v1 多文件 apply 在存储满和权限拒绝时不会
留下半升级项目。

## 注入契约

`GPUI_UPGRADE_IO_FAILURE=<stage>:<kind>` 只在测试进程中使用：

- `backup:permission_denied`：第一份精确 backup 写入返回
  `io::ErrorKind::PermissionDenied`；
- `project:storage_full`：第一个新增项目文件写入返回
  `io::ErrorKind::StorageFull`。

每个进程只注入一次，自动 recovery 的 restore 写入可以继续完成。该变量不是
公开用户配置，也不改变正常路径。

## 可复现验证

    cargo test --locked --test upgrade_io_failures -- --nocapture

每个场景都通过真实 CLI 生成 legacy project、生成 v0→v1 plan，再执行 apply；断言
包括：

- stderr 保留底层 `injected_permission_denied` / `injected_storage_full` 原因；
- apply 自动 recovery 最终为 `RolledBack`；
- replace/add/delete 受管文件和 manifest 与失败前逐字节一致；
- transaction journal 保留且为 `rolled_back`，项目锁被移除。

## 尚未覆盖

真实跨平台文件系统的磁盘配额、网络盘和管理员权限差异仍需平台验收；native
build/package 和完整 T-06/T-07 其他矩阵仍未完成，T04 继续保持 `in_progress`。
