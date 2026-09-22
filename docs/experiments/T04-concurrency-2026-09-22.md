# T04：并发编辑与锁竞争证据（2026-09-22）

状态：in_progress。本子 PR 补充 T-07 的两个 L0 事务边界：部分写入后用户
编辑不被 recovery 覆盖，以及第二个 upgrade 被项目锁拒绝。

## 覆盖范围

- 首个文件已经写成 transaction new hash 后，模拟用户再次编辑该文件；recovery
  只恢复仍归本事务所有的文件，用户新内容保留并返回 `recovery_required`。
- 已有 transaction lock 时启动第二个 apply；返回 `upgrade_busy`，不创建第二个
  transaction 目录，原 owner 的 lock 保持有效。

## 可复现验证

    cargo test --locked upgrade::apply::tests
    cargo fmt --check

结果：并发编辑保护和锁竞争测试通过；既有 stale-plan、精确 backup、用户编辑
保护与 lock owner 测试继续通过。

## 尚未覆盖

这仍是 prepared transaction 和本地并发夹具，不是第二个 embedded template
revision 的端到端迁移；跨进程 crash、磁盘满、native/toolchain validation 和
完整 T-06/T-07 矩阵继续由后续 T04 子 PR 覆盖。
