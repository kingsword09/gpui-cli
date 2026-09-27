# M04：iOS frozen build root 接入切片（2026-09-27）

状态：`in_progress`。本切片把非 live 的 iOS `build`/`run` 从“只使用稳定扫描生成
BuildKey”推进到“先创建稳定冻结副本，再从副本执行 Cargo、XcodeGen 和 xcodebuild”。
Android、live builder、持久 snapshot cache 和构建期间隐藏输入不在本切片范围内。

## 行为

- iOS build plan 先执行锁定的 Cargo metadata scope，再用
  `Inputs::freeze_to_with_cargo_scope` 创建临时 `FrozenBuildRoot`；副本包含 workspace 与
  允许的外部 Cargo path package，副本内 Cargo manifest 的 path 指向 relocated roots；
- BuildKey 的 `source_manifest_hash` 使用冻结输入 hash，`NativeInputs` 也从冻结 root
  采集，因此 Cargo 与 Xcode 配置来自同一份输入集合；
- `gpui build/run ios` 的预先 Cargo 构建从快照 root 执行；XcodeGen 在快照中的
  `mobile/ios` 生成临时 `.xcodeproj`，xcodebuild 也从该快照工程执行；
- Cargo target、DerivedData 和其他构建输出仍写入源项目
  `.gpui/builds/ios/<key>`，方便按 key 隔离复用；快照由 `TempDir` 持有，命令结束自动清理，
  源工作区不会被改写。

## 验证

- `runner::build_inputs` 测试验证 iOS build plan 使用冻结 workspace root，并使用 frozen
  input hash 生成 BuildKey；已有 simulator/device key 隔离测试继续覆盖输出布局；
- 本切片仍需要完整 workspace、clippy、设计文档和 CI 门槛；真实 iOS simulator/device
  构建只有在具备 XcodeGen、Xcode 和目标设备的 macOS 环境中才能作为平台证据。

## 未覆盖

- Android 非 live build 仍使用工作目录作为 Cargo/Gradle 工作根；
- iOS/Android live builder、持久 snapshot cache、snapshot build orchestration 和同 key
  在途任务合并尚未接入；
- `build.rs`、Gradle、Xcode 的未声明隐藏 I/O 仍不在自动发现范围内，冻结 manifest 不能
  被表述为完整构建输入闭包。
