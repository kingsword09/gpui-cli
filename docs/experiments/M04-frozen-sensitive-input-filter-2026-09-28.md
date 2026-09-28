# M04：冻结输入敏感文件名过滤切片（2026-09-28）

状态：in_progress。本切片统一 Inputs 与 NativeInputs 对两个已知 Android 本地配置文件名的
处理；不声称提供通用 secret scanner。

## 行为

- 文件名恰为 local.properties 或 keystore.properties 时，Inputs 只记录 workspace-relative
  路径到 excluded_sensitive_files，不读取文件内容、不把内容 hash 放进 source manifest；
- FrozenInputs 复制 sources/assets，但不会把上述敏感文件复制进临时冻结 root；复制核验仍
  严格对照普通输入内容，并确认目标 root 没有意外出现 excluded sensitive files；
- NativeInputs 使用相同文件名边界；BuildKey 可知道某个本地配置路径存在，但不包含其内容；
- 敏感文件内容变化不会改变 Inputs manifest；文件新增/删除会改变 excluded path 清单；
- 这也适用于使用 Inputs 的 live scan。Android CLI 冻结构建必须通过 ANDROID_HOME/
  ANDROID_NDK_HOME 提供 SDK/NDK；依赖 keystore.properties 的自定义签名配置不会被带入
  FrozenBuildRoot，需由后续安全的签名输入接线单独恢复。

## 验证

- 单元测试构造带 sdk.dir 与 signing password 的文件，验证内容未进入 manifest/hash、secret
  轮换不改变 manifest，且两个文件均未出现在冻结副本；
- workspace fmt、clippy、全量测试和设计文档检查为 PR 门槛；不将文件名 allowlist 扩大声称
  为自动发现任意项目 secrets。

## 未覆盖

- 过滤只识别这两个精确 basename；其他密钥、证书、环境文件或 Gradle/build.rs 任意读取仍未
  建模；
- 暂无对 keystore.properties 做安全的运行期 secret injection；需要此文件的自定义 Android
  release signing 在当前 FrozenBuildRoot 路径不可用；
- home 目录及 Cargo/Gradle 全部环境读取仍不是完整输入闭包。
