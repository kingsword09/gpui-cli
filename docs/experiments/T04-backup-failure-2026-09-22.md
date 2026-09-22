# T04：backup failure recovery（2026-09-22）

状态：in_progress。本子 PR 用确定性的 backup 内容冲突模拟备份写入失败，验证
失败发生在项目文件 replace 之前时，项目保持旧内容、journal 可恢复且 lock 不
会遗留。

## 覆盖范围

- 已存在且内容不同的 transaction backup 被拒绝，不覆盖既有 backup。
- apply 在 backup 阶段失败后进入 recovery；因为没有 entry 被标记为 replaced，
  项目文件保持 old bytes。
- recovery journal 最终为 `rolled_back`，项目锁释放。

## 可复现验证

    cargo test --locked upgrade::apply::tests
    cargo fmt --check

backup conflict 测试通过；既有 replace/add/delete、manifest failure、并发编辑和
toolchain validation 测试继续通过。

## 尚未覆盖

真实磁盘满/权限错误、跨进程 kill 后的 journal inspect，以及第二个 embedded
template revision 仍属于后续 T04 子 PR。
