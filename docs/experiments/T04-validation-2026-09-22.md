# T04：bounded toolchain validation（2026-09-22）

状态：in_progress。本子 PR 复用 `gpui doctor` 的 bounded probe，在 manifest
校验后对项目声明的平台执行工具链检查；不把工具缺失误报为升级成功。

## 契约

- required probe 全部通过：`validation.status=passed`。
- 工具不存在、required probe 超时或 probe 状态未知：
  `validation.status=not_run`，notes 明确 target；不宣称 native 验证完成。
- required probe 明确返回失败：apply 失败并进入精确 recovery，不提交半升级
  manifest。
- 每个实际 doctor command 以 argv 写入 validation report；不把秘密环境值写入
  journal 或 report。

当前 validation 只覆盖 bounded toolchain doctor；Cargo source check、native
package/run 仍未在本子 PR 中执行。

## 可复现验证

    cargo test --locked upgrade::apply::tests
    cargo fmt --check

完整 apply 测试继续通过；无工具链的 target 由 doctor 的 unavailable/unknown
状态进入 `not_run`，不会变成 `passed`。

## 尚未覆盖

跨平台 native build/package、磁盘满、真实第二个 template revision 和完整
T-06/T-07 证据仍需后续 T04 子 PR。
