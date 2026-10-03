# T06：Android CLI 构建与 cache-hit smoke（2026-09-28）

状态：PR #257 将 Gradle 9.4.1 官方 distribution SHA-256 纳入生成模板，并要求有效 wrapper checksum
才允许复用 artifact cache；PR #259 又将动态/changing Gradle dependency 作为 cache bypass 条件；
PR #261 将当前 host NDK 编译器和链接器内容纳入 Android toolchain fingerprint；PR #263 再纳入
host sysroot 和 Clang builtin headers 内容；PR #265 又纳入项目实际选定的 SDK platform 与
build-tools package 内容。
该 smoke 在真实 Android SDK/NDK、cargo-ndk 与 Gradle 下验证 CLI 首次构建和同 BuildKey 第二次命中；
Rust app 使用最小 cdylib fixture，不编译 GPUI UI。

## 流程

- `scripts/check-android-template.py` 先生成 Android 项目并用小型 native fixture 验证原始
  模板的 Gradle debug/release 资源和 ABI 打包；随后仅在临时项目中把 Cargo workspace/app
  收窄为无外部依赖的 `cdylib`，保留同一 gpui.toml 与 Android Gradle host；
- 使用真实 `gpui build android`、cargo-ndk 以及项目 Gradle wrapper 生成 arm64-v8a 与 x86_64
  native library 和 debug APK；断言第一次输出 cache miss，第二次输出 BuildKey cache hit；
- 检查缓存 APK 中恰好包含两个目标 ABI；设备安装与 NativeActivity 启动不在该脚本范围；
- 测试通过临时 `HOME` 和 `ANDROID_USER_HOME` 放置默认 debug keystore，Cargo/Rustup/Gradle
  工具缓存仍复用已配置目录，避免读写用户已有 signing key。
- 生成的 `gradle-wrapper.properties` 固定 Gradle 9.4.1 官方 checksum
  `2ab2958f2a1e51120c326cad6f385153bb11ee93b3c216c5fccebfdfbb7ec6cb`；BuildKey 已哈希该 properties
  文件，缺少、重复或畸形 checksum 时 CLI 仍正常构建但不消费/发布可复用 artifact manifest。
- Android cache policy 现在扫描 `mobile/android/gradle/` 下的 Gradle/Kotlin DSL 脚本和 version
  catalog，以及 `buildSrc`/`build-logic` 中的 Kotlin/Groovy/Java Gradle plugin source；动态版本
  (`1.+`)、版本范围、SNAPSHOT、latest 和 changing-module resolution 会输出 cache miss reason。
  普通 app `src/main/java`/`src/main/kotlin` 源码不在该依赖规则扫描中，避免把 ABI 集合或 API 文本
  中的 `+` 误判为动态版本。
- Android toolchain fingerprint 从项目 Gradle app 的字面量 `compileSdk` 选择对应 platform；显式
  `buildToolsVersion` 选择对应 build-tools，否则选择 SDK 中最新的数值版本。只对这些实际选定
  package 做相对路径、entry type 和内容摘要，并与 NDK 资源扫描共享 100,000 entries/512 MiB 预算；
  动态/无法解析的选择、缺包、软链接、special entry、读取失败或超预算只关闭 cache reuse。

## 证据

- 本机 macOS Android NDK 27.2.12479018 实际执行脚本通过：cargo-ndk 为两个 Rust target
  生成 `.so`，Gradle 打包 debug/release 模板 APK，GPUI CLI minimal cdylib 首次构建成功，
  第二次确认 manifest cache hit，缓存 APK ABI 为 arm64-v8a/x86_64；
- Android-template CI 安装对应 Linux Rust targets、SDK 34/build-tools 34、NDK 27.2.12479018
  和 cargo-ndk 4.1.2 后运行同一脚本；这将是 PR 门槛，不等同于完整 GPUI crate 的 Android
  编译或 emulator/device 运行。
- PR #257 本地重跑该脚本通过：Gradle wrapper 校验 pinned distribution，debug 与 release APK
  packaging 成功，最小 cdylib 的 Android CLI 第二次构建仍命中 verified BuildKey cache；399 个
  workspace 单测及集成/协议测试、clippy、fmt、build 和设计文档检查通过。PR 与 push 两套
  macOS/Windows/Ubuntu、Android/desktop template、baseline-driver CI 全绿；#257 squash 为
  `2a5782d`，无版本发布或 tag。
- PR #259 的 Android smoke 首轮暴露动态依赖扫描把模板 `abiFilters += gpuiAbis` 和普通 app
  Java 中的 `API 23+` 误判为动态版本；后续修正为只扫描 Gradle DSL、version catalog 与
  `buildSrc`/`build-logic` 插件源码，并用固定版本、动态版本、版本范围、SNAPSHOT/latest、
  changing-module 及三种插件语言回归锁定边界。最终 PR 与 push 两套 Linux/macOS/Windows、
  Android/desktop template、baseline-driver CI 全绿；#259 squash 为 `35b0afc`，无版本发布或 tag。
- PR #265 首轮 Android 强制 toolchain probe 暴露“扫描所有已安装 SDK package”会把未使用包和
  预算混入 active identity；修正为只扫描项目选定的 platform/build-tools，并增加内容替换、未使用
  package 忽略、显式/默认 build-tools、动态选择拒绝、软链接和有界预算回归。最终 PR 与 push 两套
  Linux/macOS/Windows、Android/desktop template、baseline-driver CI 全绿，Android 生成/打包验证
  通过；#265 squash 为 `2a713da`，无版本发布或 tag。

## 未覆盖

- 最小 app 主动移除了模板的 GPUI/gpui-mobile 依赖，因而不验证 GPUI renderer、窗口初始化、
  JNI 行为或完整 app 冷启动；它只验证 `gpui build android` 的冻结输入、真实 NDK 编译、
  Gradle APK 打包、manifest 校验和第二次 cache hit；
- Android emulator/device 安装启动、release/custom signing、cache 并发订阅/取消引用和容量
  清理仍未覆盖。
- wrapper checksum 只校验 Gradle distribution ZIP；它不 fingerprint Gradle runtime 的所有解压文件、
  AGP/plugins、远端仓库状态、NDK/build-script I/O，也不替代完整 Android 构建输入闭包或设备验收。
- 动态依赖扫描是保守的静态字符串检查；它能把已知不稳定声明降级为正常 cache miss，但不解析完整
  Gradle DSL，也不证明 AGP/plugin 仓库制品、远端元数据或 build-script I/O 已纳入输入闭包。
- NDK fingerprint 现在包含当前 host prebuilt 的 clang/clang++、lld/ld.lld、LLVM archiver/inspection
  tools 和 ABI-specific clang launcher 的 link target/content hash；sha2 汇编优化只用于非 Windows，
  Windows MSVC 使用纯 Rust fallback。活跃 macOS NDK 环境测试约 5 秒，canonical target hash 去重。
  #261 当时还不包含 NDK sysroot/header；该边界由下述 #263 进一步覆盖。AGP/plugin resolved artifacts
  和任意 build-script I/O 仍未纳入。
- PR #263 将当前 host 的 NDK `sysroot` 和 Clang `lib/clang/*/include` 纳入相对路径/文件类型/内容
  fingerprint；不记录绝对安装路径。扫描上限为 100,000 个 filesystem entries / 512 MiB，软链接、
special file、读取失败或超过上限会禁用 cache reuse，但普通构建继续。强制 fingerprint 测试约 9.6 秒。
  这不哈希整个 NDK、非活动 host、AGP/plugin resolved artifacts 或任意 build-script I/O。
- PR #265 仅摘要项目实际选定的 SDK platform/build-tools package，不扫描 SDK 中未使用的安装包；
  同一 package revision 下的文件替换会改变 fingerprint。Gradle SDK 选择必须是可安全解析的字面量，
  动态/无法解析时保持 cache bypass；该切片仍不验证 AGP/plugin resolved artifacts、远端仓库状态或
  任意 build-script I/O。
