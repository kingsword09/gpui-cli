# M04：iOS Xcode/SDK toolchain fingerprint 切片（2026-09-28）

状态：in_progress。本切片将当前选中的 Xcode 与目标 iOS SDK 身份加入非 live iOS
BuildKey；不宣称完整覆盖 XcodeGen、codesign 或任意 build-script 的环境读取。

## 行为

- 根据 Rust target 选择 iphoneos 或 iphonesimulator SDK，采集 xcodebuild -version、
  xcrun --show-sdk-version 与 --show-sdk-build-version；只将这些输出的 SHA-256 纳入
  NativeInputs.external_hashes，不记录 Xcode 安装绝对路径；
- Xcode/SDK identity 与 rustc -vV 摘要共同形成 iOS toolchain fingerprint；SDKROOT、
  DEVELOPER_DIR、IPHONEOS_DEPLOYMENT_TARGET 和相关 codesign 环境变量也纳入 allowlist；
- 无法完整读取目标工具链身份时仍允许 miss 后走正常构建，但禁用 iOS simulator cache hit；
- 当前本机只验证了版本探测命令输出：Xcode 26.2 / build 17C52，
  iphonesimulator SDK 26.2 / build 23C53；这不是实际 simulator app build/run 证据。

## 验证

- 单元测试证明 Xcode/SDK 指纹变化会改变 toolchain fingerprint，缺失身份产生明确的 cache
  bypass 状态；macOS CI 验证实际 xcodebuild/xcrun 探测可用；
- workspace fmt、clippy、全量测试和设计文档检查为 PR 门槛；不冒充真实 simulator 启动。

## 未覆盖

- physical-device cache hit 仍因本机签名 identity/Provisioning Profile 等输入未建模而禁用；
- XcodeGen 版本、额外 SDK/build settings、任意 Xcode build phase 隐藏读集和构建期工具变化
  尚未完整纳入 BuildKey；
- iOS live builder、跨进程任务订阅/取消、cache 清理与容量预算仍未接入。
