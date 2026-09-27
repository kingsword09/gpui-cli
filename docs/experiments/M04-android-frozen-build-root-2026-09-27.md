# M04：Android frozen build root 接入切片（2026-09-27）

状态：`in_progress`。本切片把非 live 的 Android `build`/`run` 从“只使用 ABI 对应的
BuildKey 和隔离输出”推进到“先创建稳定冻结副本，再从副本执行 cargo-ndk 和 Gradle”。
live Android、持久 snapshot cache、同 key 在途任务合并和构建期间隐藏输入不在本切片范围内。

## 行为

- Android build plan 先执行锁定的 Cargo metadata scope，再用
  `Inputs::freeze_to_with_cargo_scope` 创建临时 `FrozenBuildRoot`；副本包含 workspace 与
  允许的外部 Cargo path package，副本内 Cargo manifest 的 path 指向 relocated roots；
- BuildKey 的 `source_manifest_hash` 使用冻结输入 hash，`NativeInputs` 从冻结 root 采集，
  ABI 集合仍规范化为稳定顺序；
- `gpui build/run android` 的 cargo-ndk 从冻结 workspace root 执行，Gradle wrapper 从
  快照中的 `mobile/android/gradle` 执行；JNI staging、Gradle build 目录和 Cargo target
  仍写入源项目 `.gpui/builds/android/<key>`；
- 临时快照由 `TempDir` 持有，命令结束自动清理，源工作区和源项目的 JNI/Gradle 输出不被
  改写；Gradle 通过已有 `gpui.jniLibsDir`/`gpui.buildDir` 参数读取 key 私有输出。

## 验证

- `runner::build_inputs` 测试验证 Android build plan 使用冻结 workspace root、frozen input
  hash、规范化 ABI 集合和 Android key 输出布局；命令测试验证 Gradle wrapper 的 current
  directory 和程序路径可以切换到快照工程；
- 本切片仍需要完整 workspace、clippy、设计文档和 CI 门槛；真实 Android emulator/device
  安装启动只有在具备 SDK/NDK/JDK 和目标设备的环境中才能作为平台证据。

## 未覆盖

- Android/iOS live builder、持久 snapshot cache、snapshot build orchestration 和同 key
  在途任务合并尚未接入；
- `build.rs`、Gradle、NDK 的未声明隐藏 I/O 仍不在自动发现范围内，冻结 manifest 不能被
  表述为完整构建输入闭包；
- 本切片不改变 Android ABI 矩阵、设备租约或安装/运行身份语义。
