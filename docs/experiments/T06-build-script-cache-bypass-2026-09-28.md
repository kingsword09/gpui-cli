# T06：未建模 `build.rs` 输入的 cache bypass 切片（2026-09-28）

状态：in_progress。本切片针对 Cargo workspace 与冻结 external path package 中未建模的 Cargo
build script，保守关闭 BuildKey artifact cache reuse，但仍允许冻结和正常构建。

## 行为

- 在 desktop、iOS、Android 的 frozen build plan 中扫描快照 Cargo package manifest 与源码清单；无显式
  `package.build` 时，package 内存在 `build.rs` 才给 cache lookup bypass；`build=false` 不触发；字符串
  自定义脚本路径或 `build=true` 仍触发 bypass；
- Cargo metadata 发现的 external path package 与 workspace 一起进入冻结副本，因此外部 package 也按同一
  manifest 语义判定；
- 不把 `build.rs` 的任意实际读取集假装成已知输入，也不阻止 cargo/xcode/Gradle 正常执行；
  BuildKey 仍保留源码 manifest/native/toolchain 等已建模维度，用于输出隔离与构建证据；
- cache bypass 在锁内打印 miss 原因，并仍发布完成产物 manifest；后续命令会继续正常构建，
  直到 build-script 输入有明确建模；
- registry/git 依赖中 Cargo 管理的 build script 不触发本地 workspace 规则，当前只防止项目
  自身或被冻结 external path package 的隐藏读取集造成误命中。

## 验证

- 单测验证默认 `build.rs`、`build=false`、自定义脚本路径和 external custom script 的 bypass 边界；无
  build script 时保持可命中；
- workspace fmt、clippy、全量测试和设计文档检查作为 PR 门槛；未宣称对 build.rs 读取集自动
  发现，也未把正常构建误报成 cache hit。

## 未覆盖

- 尚未追踪 `build.rs` 的实际文件/环境读取，也未将声明式 `rerun-if-changed` 闭包完整纳入；
- Gradle/NDK/XcodeGen/Xcode build phase 隐藏输入、同 key 在途任务共享和远程缓存仍未接入；
- cache bypass 不等于构建可复现证明，只是防止当前证据不足时错误复用旧产物。
