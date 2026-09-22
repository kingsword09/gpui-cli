# T04：第二个 embedded template revision 迁移（2026-09-22）

状态：in_progress。本子 PR 为真实 CLI 引入可重复生成的
`agent-native-v0-legacy` baseline，并验证迁移到 `agent-native-v1-draft` 时同时
产生 replace、add、delete；不将 T04 提前标记为完成。

## Revision 设计

- `agent-native-v1-draft` 是当前模板，包含受管的
  `crates/app/src/agent_runtime.rs`。
- `agent-native-v0-legacy` 使用当前嵌入模板的稳定内容摘要，加上固定历史
  overlay：删除 `agent_runtime.rs`、加入 `legacy_runtime.rs`，并给 `lib.rs`
  加入历史前缀。
- legacy manifest 的 file hashes、groups、platforms、dependencies 和 baseline
  reference 必须与 CLI 在同一版本重新生成的 embedded scaffold 完全一致；
  base validation 会拒绝伪造或不完整的历史 manifest。

## 可复现验证

    cargo test --locked --test upgrade_revision_migration -- --nocapture
    cargo test --locked upgrade::tests::legacy_embedded_baseline_produces_real_replace_add_and_delete_plan

端到端测试先通过 CLI `init` 生成项目，再构造 legacy manifest，执行：

    gpui upgrade plan --to agent-native-v1-draft --json
    gpui upgrade apply --plan <plan_id> --json

结果：

- plan status 为 `ready`，base/target 分别为 v0/v1；
- file plan 含 1 个 replace、1 个 add、1 个 delete；
- apply 至少提交这 3 个文件以及最后提交的 manifest；
- `legacy_runtime.rs` 被删除，`agent_runtime.rs` 被加入，历史 `lib.rs` 前缀被移除；
- `manifest.template_version` 最后为 `agent-native-v1-draft`；
- bounded validation 为 `passed` 或工具链不可用时的 `not_run`；
- apply 结束后项目升级锁被移除，已提交 transaction journal 保留可审计。

## 尚未覆盖

完整 T-06/T-07 故障点矩阵、磁盘满/权限错误、跨平台 native build/package，以及
更多真实历史模板版本仍需后续 T04 子 PR；T04 继续保持 `in_progress`。
