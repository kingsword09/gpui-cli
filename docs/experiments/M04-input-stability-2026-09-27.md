# M04：live 输入稳定扫描第一切片（2026-09-27）

状态：`in_progress`。本切片交付 workspace content manifest 的有界稳定扫描和
root 外的受校验副本；尚未声称完成 Cargo metadata 外部依赖发现、BuildKey 或独立
构建输出隔离。

## 行为

- `Inputs::scan_stable(root, max_rescans)` 先扫描一次，再要求连续两次扫描内容完全
  相同；内容比较覆盖 sources、assets 和未跟踪目录 symlink 声明；
- live session 当前使用最多 2 次 rescan。单次文件竞态可在边界内收敛，持续编辑
  返回明确错误 `project inputs changed during the bounded stability scan`；
- 稳定扫描失败不会更新 session 的 source/asset revision，也不会把本轮 observe 绑定
  到混合输入；
- 该逻辑仍是 `tracked_scan`，不是文件系统快照：build.rs 隐藏读集、外部 path
  dependency 和构建期间的后续编辑仍属于 M04 后续范围。
- `Inputs::freeze_to` 要求 snapshot 目标不存在且位于 source root 外，拒绝目录 symlink
  范围，复制后重新扫描 snapshot，并再次扫描 source；任一 hash/manifest 不一致都
  删除本次新建目录并失败。

## 验证

- 单元测试覆盖一次变化后收敛，以及超过重扫上限仍变化时拒绝；
- 单元测试覆盖 root 外 snapshot 复制、ignored output 不复制、root 内目标拒绝和
  snapshot manifest/hash 重核验；
- 原有 asset add/change/delete 与 ignored output 行为继续通过；
- workspace clippy 通过。

## 未覆盖

完整 M04 仍需要冻结输入副本、Cargo metadata/path dependency 范围、toolchain/profile/
feature/ABI/native/env BuildKey、同 key 构建目录隔离和外部链接越界拒绝。这些能力
完成前，严格 check/matrix 不得把 `tracked_scan` 提升为 `frozen_snapshot`。
