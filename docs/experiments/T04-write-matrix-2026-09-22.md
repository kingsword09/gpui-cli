# T04：apply writer mutation matrix（2026-09-22）

状态：in_progress。本子 PR 只补 prepared transaction writer 的真实写入路径
和 manifest 阶段故障证据，不把当前单一 embedded baseline 误称为版本迁移完成。

## 覆盖范围

- 在同一个 journal 中执行 `replace`、`add`、`delete`，每个 entry 都按 old hash
  重核、写入后按 new hash 校验。
- manifest 写入前失败时不产生文件修改；manifest 写入后失败时从精确 backup
  恢复旧内容。
- 两种 manifest 故障恢复后都保留 `rolled_back` journal 状态，未使用 Git reset
  或宽泛目录删除。

## 可复现验证

    cargo test --locked upgrade::apply::tests
    cargo fmt --check

新增测试结果：replace/add/delete writer matrix 和
`BeforeManifest`/`AfterManifest` 两个故障点均通过。

## 尚未覆盖

当前 `agent-native-v1-draft` 是唯一 embedded target，CLI 端到端运行仍是 no-op
apply；第二个真实 template revision、native/toolchain validation、磁盘满和
跨进程并发编辑证据仍属于后续 T04 子 PR。
