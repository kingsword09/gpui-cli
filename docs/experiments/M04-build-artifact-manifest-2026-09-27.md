# M04：构建产物 manifest 校验切片（2026-09-27）

状态：`in_progress`。本切片补齐后续构建缓存所需的“完整产物才可复用”基础契约，暂不
改变现有 build/live 命令的调度和缓存行为。

## 行为

- `src/runner/build_manifest.rs` 提供 schema-v1 的 `BuildArtifactManifest`，绑定
  `BuildPlatform` 和完整 BuildKey hash；每个条目包含规范化相对路径、文件大小、SHA-256
  和 executable 标志，并保留规范化声明根；manifest 自身不进入文件列表，避免自引用；
- capture 支持声明文件或目录，目录递归展开并按路径排序；空输出、越界路径、符号链接和
  特殊文件都会拒绝；
- `verify` 逐条检查 regular-file、存在性、大小、内容 hash 和 executable 标志；
  同时重扫声明目录以拒绝新增的未登记文件；`read_verified` 还会先校验 schema、平台和
  BuildKey 绑定，不能只因为目录存在就报告命中；
- `write_atomic` 在目标目录创建临时文件、同步后原子发布，读取者不会观察到半写入的 JSON；
  `BuildOutputLayout` 暴露 key 私有的 `artifact-manifest.json` 路径。

## 验证

- 单元测试覆盖目录递归/排序与内容篡改、新增/缺失产物、原子 round-trip/platform/BuildKey
  绑定、越界路径、父目录 symlink 逃逸和 manifest 自引用拒绝；
- 本切片门槛包括 workspace fmt、clippy、全量测试和设计文档检查；真实平台构建产物尚未
  由 build 命令自动发布 manifest。

## 未覆盖

- desktop/iOS/Android 命令尚未在构建成功后声明并发布各自的最终 artifact manifest；
- 持久构建缓存、同 key 在途任务合并、引用计数清理、取消传播和有界预热尚未接入；
- `build.rs`、Gradle、NDK、Xcode 的未声明隐藏 I/O 仍不在输入闭包内。
