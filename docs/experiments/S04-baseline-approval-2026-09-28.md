# S04：显式 baseline review/approval（2026-09-28）

状态：in_progress。本切片提供独立的人工选择入口，不由 check、diff 或修复流程自动调用。

## 命令

    gpui baseline approve +      --target macos +      --baseline-id counter-basic +      --key .gpui/checks/baseline-key.json +      --image .gpui/checks/approved.png +      --diff .gpui/checks/counter-basic.diff.png +      --reason "reviewed on the fixed macOS runner" +      --json

命令要求完整 BaselineKey、可解码 PNG 和非空 review reason。目标/ID、key、PNG、diff
路径都经过边界和 symlink 检查；manifest、image、approval record 先写入 staging 目录，
再原子发布到 dev/baselines/target/id。

重复 approve 默认拒绝。显式传入 replace 时，旧 baseline 整个目录先移动到
dev/baselines/target/.history/id/revision，新的目录再发布；approval.json 保存旧 manifest、
history 相对路径、new manifest、reason、时间、可选 diff 的 hash/路径。发布失败会尝试
把旧目录移回，不执行宽泛删除。

## 限制

这不是自动更新开关，也不改变 screenshot_matches 的结果。命令不会从当前 live session
猜测截图、放宽 key、扩大 mask 或替换失败断言；用户必须显式选择 image/key/reason。
tolerance、mask 配置和跨平台矩阵审批仍未实现。

## 验证

- 首次 approve 写出 manifest/image/approval。
- 重复 approve 无 replace 时拒绝。
- replace 保留旧 manifest/image 到 history，并在新 approval 中引用旧 revision。
