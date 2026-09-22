# T04：验证失败回滚（2026-09-22）

状态：in_progress。本子 PR 补充 T-06 中“验证失败不能提交半升级 manifest”的
真实 CLI 证据；它与 before/after validation 的进程 crash 注入分开验证。

## 场景

在 v0→v1 多文件迁移已经写完 replace/add/delete 和 manifest 后，隐藏测试变量
让 bounded validation 返回明确的 `injected_validation_failure`。apply 必须进入
精确 recovery，不能把目标 manifest 或部分文件留下作为新基线。

## 可复现验证

    cargo test --locked --test upgrade_io_failures -- --nocapture

新增场景断言：

- apply 失败信息包含 `injected_validation_failure` 和
  `recovery_state=RolledBack`；
- replace/add/delete 文件及 manifest 与写入前逐字节一致；
- rolled-back journal 保留，项目锁被移除。

## 尚未覆盖

真实 native build/package 失败、跨平台工具链矩阵和 C-02 的 F02 feature/release
边界仍需后续工作；T04 继续保持 `in_progress`。
