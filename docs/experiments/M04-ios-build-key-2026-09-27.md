# M04：iOS BuildKey 输出接入切片（2026-09-27）

状态：`in_progress`。本切片把非 live 的 `gpui build ios` / `gpui run ios` 接到真实
BuildKey：Cargo target 与 Xcode DerivedData 都按 iOS target/profile 隔离；不宣称 Android、
live 模式或冻结快照构建已经接入。

## 行为

- `ios_build_key` 使用稳定源码 manifest、`Cargo.lock`、`NativeInputs`、`rustc -vV` 的
  toolchain 摘要、显式 iOS Rust target、profile 和环境 allowlist；
- `aarch64-apple-ios-sim` 与 `aarch64-apple-ios` 生成不同 BuildKey，因此 simulator 与
  physical-device 构建不会共用 Cargo target 或 DerivedData；
- iOS 构建布局为 `.gpui/builds/ios/<key>/cargo-target` 与
  `.gpui/builds/ios/<key>/native-staging/ios/derived-data`；
- 预先执行的 Cargo 构建和 `xcodebuild` 都设置 `CARGO_TARGET_DIR`，因此 Xcode
  `preBuildScripts` 中的 Rust Cargo 调用也继承同一 target；`xcodebuild` 同时使用
  `-derivedDataPath` 指向 key 私有目录，并以 `GPUI_CARGO_TARGET_DIR=...` 覆盖 linker
  使用的静态库根目录；
- iOS 模板中的 `GPUI_CARGO_TARGET_DIR` 默认仍指向项目根 `target/`，所以用户直接在
  Xcode 中构建生成项目时保持原有行为；CLI 构建才覆盖为 BuildKey 私有目录；
- XcodeGen 仍在 `mobile/ios` 生成 `.xcodeproj`，该生成文件属于受控输入范围之外的构建
  产物，不改变源项目文件的职责边界。

## 验证

- `runner::build_inputs` 测试验证 device/simulator target 产生不同 key、iOS layout 位于
  `.gpui/builds/ios/<key>`、只提供 DerivedData 而不提供 Android JNI staging；
- 本 PR 门槛结果：`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo test --workspace --locked`（221 个 unit tests 及全部 integration/doc tests）和
  `cargo x check-design-docs` 均通过；设计文档检查报告 81 个 Markdown 文件、112 个
  本地链接、35 个任务和 68 个验收用例。真实 iOS simulator/device 启动仍需平台验收，
  缺少工具或设备时不能伪造成功证据。

## 未覆盖

- `gpui live ios` 仍使用 live builder 的工作目录输出，尚未接入 BuildKey；
- Android 多 ABI 的 Cargo/JNI/Gradle staging、snapshot build orchestration、缓存命中和
  同 key 在途任务合并尚未接入；
- `build.rs`、Gradle、Xcode 的未声明隐藏 I/O 仍不在自动发现范围内，desktop/iOS 接入
  不能被表述为完整输入闭包或跨平台可复现构建。
