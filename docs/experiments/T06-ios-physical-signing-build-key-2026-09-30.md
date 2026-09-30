# T06：iOS physical signing BuildKey 输入（2026-09-30）

状态：PR #203 已 squash 合并为 `d20dadc`。本切片把 physical iOS build/run 的本机签名状态纳入
BuildKey，并在 artifact manifest 复用前后验证它；不保存证书、profile 或私钥内容。

## 实现范围

- macOS `security find-identity -v -p codesigning` 的有效 identity 指纹集合，以及用户目录下可读的
  `.mobileprovision` 文件内容摘要，经过排序去重后组合为 `ios.physical-signing` 哈希。
- physical iOS `BuildKey` 的 `NativeInputs.external_hashes` 纳入该摘要；identity/profile 不可读或为空时
  写入不可用标记并禁用 cache reuse。非 macOS/模拟器路径不触发该 physical signing probe。
- physical build 在 coordinator/cache lookup 前、Xcode 构建前后以及 manifest 发布前后重核验签名摘要；
  中途变化直接失败，不发布可复用 artifact manifest。
- physical manifest 现在可在签名输入可用且 BuildKey 匹配时命中；签名输入不可用时仍按 cache miss
  构建。模拟器行为不变；Android release/custom/signing-sensitive 路径仍 bypass。

## 验证

- 本地通过 366 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs 和 diff check。
- 新增签名 probe 可用性、不可用降级、哈希归一化和签名变化导致 NativeInputs digest 变化的回归。
- PR/push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 和文档
  门槛均通过。未发布版本，未创建 tag。

## 未覆盖与下一步

当前摘要覆盖本机证书 identity 列表和 provisioning profile 文件集合，不解析 profile entitlement、bundle
identifier、team 匹配或证书私钥可用性，也不建模 Xcode 自动签名服务/远端 profile 更新的全部状态。Android
release/custom signing、Gradle plugin/wrapper 隐藏输入和真实 physical device 连续验收仍未完成。下一步
继续 Android 签名敏感边界与移动真实场景证据。未发布版本，未创建 tag。
