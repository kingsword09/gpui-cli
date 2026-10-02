# T06：Android local custom/release signing BuildKey（2026-09-30）

状态：PR #205 已 squash 合并为 `142f58b`，PR #207 已 squash 合并为 `d90f66b`，PR #209 已 squash
合并为 `a8a484f`，PR #249 已 squash 合并为 `90585ef`，PR #255 已 squash 合并为 `a5ab033`。受控 release-only signing 配置现在可用于
debug live preview 的签名感知缓存；Android release APK 和复杂/远端 signing 仍不复用 preview cache。
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
  并把 fingerprint 传入 preview 子进程；cargo-ndk/Gradle 前后复核该 fingerprint。显式
  `buildTypes.debug.signingConfig` 的 custom-debug preview 会把该 fingerprint 纳入 preview BuildKey，
  通过 verified JNI/APK manifest 发布与消费，并进入 preview coordinator。PR #249 扩展了 debug 变体
  的受控情况：若静态确认 custom `signingConfig` 只绑定 release build type，debug preview 继续采用
  默认 debug keystore；preview BuildKey 同时纳入 release properties/keystore fingerprint 与默认
  debug keystore hash，且重验两组输入。此复用只适用于 debug preview，不表示 release APK 可缓存；
  缺失默认 debug keystore、debug signing 状态无法静态判定、复杂 DSL、多脚本/插件或远端 signing
  仍保持 cache bypass。
- PR #255 将 signing marker 扫描扩展到 Gradle root 内 Kotlin/Groovy/Java 源码，避免 buildSrc 或
  convention plugin 在 app build script 之外设置 variant signing 时被误判为 default-debug；同时，
  `settings.gradle(.kts)` 使用 `includeBuild` 时不论自定义签名是否直接可见都禁用 cache reuse，因为
  included build 的 plugin 实现可能在 workspace 外。两条路径都只降级为正常构建/cache miss，不阻止
  冻结构建或签名输入快照。

## 验证

- 单元测试覆盖签名 identity 摘要与变化重验、项目内 snapshot 注入和 `0600` 权限、secret 不进入
  manifest、项目外路径、软链接、复杂多脚本 signing bypass、Gradle plugin signing source 与
  `includeBuild` cache bypass，以及 keystore 变化导致 native digest 变化。
- 本地通过 `cargo test --workspace --locked`（375 个单元测试及全部集成/协议测试）、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、workspace fmt、
  `cargo x check-design-docs` 和 `git diff --check`。
- PR #209 的两套 required CI 中 Linux/macOS/Windows check、desktop-template、android-template、
  baseline-driver 全部通过。没有发布版本或创建 tag。
- PR #249 新增三态 debug signing 判定与 release-only 双签名输入绑定回归测试；本地通过
  `cargo test --workspace --locked`（393 个单元测试及全部集成/协议测试）、
  `cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo check --workspace --locked`、
  workspace fmt、`cargo x check-design-docs` 和 `git diff --check`。PR 与 push 两套 CI 的
  Linux/macOS/Windows check、desktop-template、android-template、baseline-driver 全部通过。
  PR #249 squash 为 `90585ef`；没有发布版本或创建 tag。
- PR #255 的 Gradle plugin/included-build bypass 回归在本地通过，完整 workspace 有 397 个单元测试
  和全部集成/协议测试通过；`cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo fmt --check`、`cargo build --locked` 与 `git diff --check` 通过。PR 与 push 两套 CI 的
  macOS/Windows/Ubuntu check、Android/desktop template 和 baseline-driver 全绿；PR squash 为
  `a5ab033`，没有发布版本或创建 tag。

## 未覆盖

- Gradle wrapper distribution、AGP/plugin 隐藏读取、NDK/build-script I/O、远端 signing 服务、私钥
  可用性和同 revision 工具包内容变化仍未形成完整输入闭包。
- Android live preview 的 default-debug signing、显式 debug custom signing 与受控 release-only
  signing 配置下的 debug variant 已进入 verified manifest/coordinator；release APK、复杂/远端 signing
  仍不发布可复用 preview manifest，也不进入 preview coordinator。
- #255 的 Kotlin/Groovy/Java marker 扫描是保守字符串检查；Gradle wrapper distribution、AGP/plugin
  内部 I/O、NDK/build-script 隐藏输入和任意远端签名输入闭包仍未完整建模。`includeBuild` 只触发 bypass，
  没有复制/哈希外部 build logic 或建立其远端依赖身份。
- 本切片没有新增 emulator/device 安装、启动、capture 或完整 scenario 连续验收证据。

## 关联的移动 scenario evidence

PR #211（squash `058d2c6`）修正了 Android preview 被 `gpui check --matrix` 消费时的 lease/evidence
归属：matrix supervisor 持有唯一设备 OS lease，preview 子进程只接收 owner path、session 和
fencing token 摘要的 delegated view；capture、native logs、stop evidence 绑定 preview 实际 run，
并进入 `CheckContext.mobile_evidence`。这使显式 debug custom-signing preview 的产物消费与设备证据
可以分开审计；PR #249 另外允许受控 release-only keystore 配置下复用 debug preview 产物，但不增加
release APK/复杂或远端 signing cache，也不构成真实设备矩阵或完整语义/输入验收。
