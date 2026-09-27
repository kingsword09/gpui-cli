# M04：BuildKey 输出目录隔离切片（2026-09-27）

状态：`in_progress`。本切片把 `(platform, BuildKey)` 映射到独立的 Cargo target、native
staging、Android JNI 或 iOS DerivedData 路径；尚未让现有 build 命令使用这些路径。

## 行为

- 输出根为 `base/<platform>/<key_hash>/`，相同 key 和平台得到相同路径，不同 key 不共享；
- 所有平台都有独立 `cargo-target` 和 `native-staging`；
- Android 要求 BuildKey 携带 ABI，并把 JNI staging 再按 ABI 分开；
- iOS 使用 key 私有的 `derived-data`，不复用 Android JNI 或项目可变目录；
- `prepare` 只创建当前 layout 拥有的目录，不清理或覆盖其他 key 的输出；
- key hash 和 ABI 都作为受限路径段处理，relative base、缺失 Android ABI 和路径逃逸被拒绝。

## 验证

- 同一个 key 的布局可重复生成，Android JNI 路径包含 ABI；
- 不同 key、不同平台的 root 不相同；
- Desktop 不产生移动端 staging 路径；
- 输出布局可序列化为包含 platform、key hash 和具体目录的证据；
- `prepare` 创建的目录存在且只位于当前布局下。

## 未覆盖

本切片没有改造 `cargo build`、Gradle、xcodebuild 或现有 build 命令，也没有实现缓存命中、
并发构建合并、取消引用计数和快照中的外部 path dependency relocation。
