# M04：Android 构建产物 manifest 发布切片（2026-09-28）

状态：`in_progress`。本切片把 artifact manifest 接入 Android 非 live `build`/`run` 的成功
路径；不启用缓存命中，也不改变 live Android。

## 行为

- 在 cargo-ndk 成功、每个 ABI 的应用 `.so` 均存在、Gradle 成功并从 output metadata 找到
  APK 后，CLI 才发布 `.gpui/builds/android/<key>/artifact-manifest.json`；任一前置步骤失败
  都不会发布新的成功 manifest；
- manifest 绑定 Android platform 和 ABI 集合 BuildKey，并覆盖 key 私有 JNI staging 全目录
  及当前 variant 的 Gradle APK 输出目录，包含 APK 和 `output-metadata.json`；
- manifest 通过同目录临时文件原子写入。后续 `read_verified` 会检查两棵声明根内的精确
  文件集合，因此缺失、同大小内容变化、额外文件或 symlink 都拒绝验证；
- `gpui run android` 仍在 build 后创建新的设备安装/运行流程，artifact manifest 不复用
  安装身份。

## 验证

- 命令单元测试用临时 BuildKey layout 写入 JNI、APK 和 output metadata，验证生成的 manifest
  可按 Android platform/BuildKey 完整读取；往 APK 输出目录增加未登记文件后验证失败；
- 本 PR 门槛包括 workspace fmt、clippy、全量测试、设计文档检查和 GitHub 平台 CI；真实
  Android emulator/device 启动需要具备 SDK/NDK/JDK 与设备的环境，不由该 fixture 冒充。

## 未覆盖

- desktop/iOS build 命令尚未发布 artifact manifest；live builder 也未接入；
- manifest 校验尚不触发 cache hit；持久缓存、相同 key 在途任务合并、取消引用和清理尚未接入；
- Gradle/NDK 未声明的外部文件或环境读取不在 frozen input 闭包内。
