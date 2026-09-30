# T06：Android local custom/release signing BuildKey（2026-09-30）

状态：PR #205 已 squash 合并为 `142f58b`；后续 live frozen-preview 接线仍在当前实现切片中。
本切片为 Android `build`/`run` 和 matrix/live frozen build 补齐一段可证明的本地 custom/release
signing 输入边界；不把复杂 Gradle、远端插件或 signing-sensitive 输出标为可复用。

## 实现范围

- 只识别项目内固定位置 `mobile/android/gradle/keystore.properties`，并要求 app 的
  `build.gradle`/`build.gradle.kts` 直接出现受控的 `signingConfigs`、`storeFile`、
  `keystoreProperties.load` 和 `signingConfig` 形态；其他脚本、应用脚本、provider 或复杂/远端
  signing 继续 cache bypass。
- `storeFile` 只接受相对路径，解析候选基准为 Android app/Gradle/project root；最终必须是项目根内的
  regular file，且只接受 `.jks`、`.keystore`、`.p12`、`.pfx` 扩展名。项目外、绝对路径、软链接、
  缺失、畸形或歧义输入不会进入可复用 BuildKey。
- `keystore.properties` 全文摘要、keystore 内容摘要和项目相对路径组合成
  `android.custom-signing` external hash；BuildKey 不保存密码、properties 内容或 keystore 字节。
  常见 keystore 扩展名进入敏感输入排除边界，不被普通 source/native manifest 哈希。
- 规划成功后，仅把批准的 properties/keystore 复制进短生命周期 FrozenBuildRoot，Unix 权限设为
  `0600`；snapshot 的公开 manifest 只保留排除路径，Gradle 读取副本，原项目文件不被改写。
- custom debug 与已配置签名的 release build/run 都在 BuildKey 匹配时允许 artifact manifest 命中。
  cache lookup 前、Gradle 前后、manifest 发布前后和最终消费前均复核 signing inputs；建模的 release
  若产出 `-unsigned.apk`，不会作为签名敏感缓存继续发布/消费。
- matrix frozen Android preview 会把同一组批准的 properties/keystore 以 `0600` 副本注入 snapshot，
  并把 fingerprint 传入 preview 子进程；cargo-ndk/Gradle 前后复核该 fingerprint。custom signing
  preview 允许正常构建和运行，但不发布/消费 preview artifact manifest，也不进入 preview coordinator。

## 验证

- 单元测试覆盖签名 identity 摘要与变化重验、项目内 snapshot 注入和 `0600` 权限、secret 不进入
  manifest、项目外路径、软链接、复杂多脚本 signing bypass，以及 keystore 变化导致 native digest
  变化。
- 本地通过 `cargo test --workspace --locked`（370 个单元测试及全部集成/协议测试）、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、workspace fmt、
  `cargo x check-design-docs` 和 `git diff --check`。
- PR #205 的两套 required CI 中 Linux/macOS/Windows check、desktop-template、android-template、
  baseline-driver 全部通过。没有发布版本或创建 tag。

## 未覆盖

- Gradle wrapper distribution、AGP/plugin 隐藏读取、NDK/build-script I/O、远端 signing 服务、私钥
  可用性和同 revision 工具包内容变化仍未形成完整输入闭包。
- Android live preview 只有 default-debug signing 进入 verified manifest/coordinator；custom/release/
  signing-sensitive preview 现在可以在受控 frozen snapshot 中构建，但仍不发布可复用 manifest 或
  进入 preview coordinator。
- 本切片没有新增 emulator/device 安装、启动、capture 或完整 scenario 连续验收证据。
