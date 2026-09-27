# M04：native manifest 输入范围切片（2026-09-27）

状态：`in_progress`。本切片定义 native host 输入的受控文件范围和 content hash，供后续
BuildKey/native config 采集使用；不改变现有构建命令。

## 行为

- `NativeInputs::scan` 收集 `gpui.toml`、`.cargo/config.toml`、iOS `mobile/ios` 和
  Android `mobile/android/gradle` 的 manifest、脚本、资源和源码；
- 忽略 `build/`、`.gradle/`、`jniLibs/`、生成的 `.xcodeproj`/`.xcworkspace` 和其他
  构建输出；
- `local.properties`、`keystore.properties` 等本地敏感配置不进入 manifest，而是记录为
  `excluded_sensitive_files`；
- 目录 symlink 不遍历并记录，文件 symlink 直接拒绝；文件按规范化相对路径和 SHA-256
  排序生成 digest；缺失的 iOS/Android root 对 desktop-only 项目是合法的。

## 验证

- native manifest、iOS asset、Gradle script、Android `.cargo` 配置被收集；
- 生成目录、JNI staging 和敏感 local properties 被排除；
- native digest 可由序列化 manifest 稳定产生。

## 未覆盖

本切片尚未把 native digest 接入 BuildKey，也没有解析原生工具实际读取的 build.rs/Gradle
隐藏输入、签名秘密来源或构建调度。
