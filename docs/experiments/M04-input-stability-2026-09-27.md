# M04：live 输入稳定扫描第一切片（2026-09-27）

状态：`in_progress`。当前记录 workspace content manifest 的有界稳定扫描、root
外的受校验副本，以及 Cargo metadata 的外部 path package 范围发现；尚未声称完成
外部依赖副本重定位、BuildKey 或独立构建输出隔离。

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
- `CargoInputScope::discover` 执行完整的 `cargo metadata --format-version 1 --locked`，
  只把 `source = null` 的本地 Cargo package 视为项目输入候选；registry/git package
  不纳入外部范围。
- 外部 package 按 canonical root 排序并去重，保留 manifest path 和 package id 证据；
  发现到包含 workspace root 的外部祖先或 symlinked package root 时拒绝。
- 本切片只产出有序的 Cargo scope，尚未把外部 root 合并进 `Inputs`/`FrozenInputs`，
  也尚未重写快照中的 path dependency。

## 验证

- 单元测试覆盖一次变化后收敛，以及超过重扫上限仍变化时拒绝；
- 单元测试覆盖 root 外 snapshot 复制、ignored output 不复制、root 内目标拒绝和
  snapshot manifest/hash 重核验；
- 单元测试覆盖 Cargo metadata 的外部 path package 排序/去重、registry source 过滤、
  workspace root 越界拒绝、workspace 不匹配拒绝；当前 workspace 的真实
  `cargo metadata --locked` 调用也通过；
- 原有 asset add/change/delete 与 ignored output 行为继续通过；
- workspace clippy 通过。

## 未覆盖

完整 M04 仍需要把外部 path dependency 纳入冻结副本并完成 path relocation、场景/native
manifest 输入、toolchain/profile/feature/ABI/native/env BuildKey、同 key 构建目录隔离和
构造期隐藏输入限制。这些能力完成前，严格 check/matrix 不得把 `tracked_scan` 提升为
`frozen_snapshot`。
