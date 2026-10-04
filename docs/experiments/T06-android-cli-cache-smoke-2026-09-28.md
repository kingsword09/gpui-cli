# T06：Android CLI 构建与 cache-hit smoke（2026-09-28）

状态：PR #257 将 Gradle 9.4.1 官方 distribution SHA-256 纳入生成模板，并要求有效 wrapper checksum
才允许复用 artifact cache；PR #259 又将动态/changing Gradle dependency 作为 cache bypass 条件；
PR #261 将当前 host NDK 编译器和链接器内容纳入 Android toolchain fingerprint；PR #263 再纳入
host sysroot 和 Clang builtin headers 内容；PR #265 又纳入项目实际选定的 SDK platform 与
build-tools package 内容；PR #267 为 Gradle Android plugin/dependency artifacts 加入 SHA-256
dependency verification metadata，并把严格校验状态作为 cache reuse 前置条件；PR #269 又纳入
实际 Java runtime 内容。PR #295 为已知 Gradle global user-home 配置/注入环境增加 cache bypass；PR #296
禁止 cache-disabled Android build/preview 发布新的可复用 artifact manifest；PR #298 检查 wrapper 解压
  distribution 的 `init.d` 自定义脚本入口；PR #300 将标准 layout 下所有已安装 distribution 内容纳入
  Android toolchain fingerprint/BuildKey；PR #302 又将该 identity 传入普通 build 与 matrix/live preview，
  在复用前和 Gradle 完成后重核验；PR #304 对本地 buildSrc/build-logic 自定义 Gradle plugin 输入直接 bypass。

## 后续 T05 输入索引证据（2026-10-03）

该实验文件继续作为本轮三文件文档切片的证据索引；T05 代码并不改变 Android smoke 的设备范围。

- `17b369d`（PR #271）加入路径/hash/mtime/大小/file identity 索引，dirty、rename/delete 双向失效，
  overflow、incomplete rename 和读取竞态回退；`a995989`（PR #272）将其接入 live watcher/session。
- build/observe/显式 sync 仍执行稳定全量核验；索引只优化 watcher 增量刷新，不改变
  `wrong_revision_acceptance` 边界。
- `c2ddaa3`（PR #273）加入 ignored 手动 benchmark。Apple M2/macOS/aarch64、release profile、
  4096 个 8192-byte 文件（32 MiB）、10 次 warmup + 30 次交替测量：oracle mismatch=0、
  `wrong_revision_acceptance=0`；索引更新 P95 约 1.280 ms wall/1.242 ms process CPU，全量稳定
  oracle P95 约 170.913/170.458 ms；单文件更新 hash 读取 16 KiB，oracle 稳定双扫描读取 64 MiB。
- PR #275（`de97a17`）将 Cargo metadata 发现的外部 path-package roots 纳入 live session 输入索引与
  watcher；外部逻辑 slot 按稳定 package identity 排序，不公开绝对 source root；Cargo.toml 变化刷新
  scope 并增删 watcher roots。workspace 与 external 同名逻辑路径保持独立；冻结副本 slot 与 workspace
  路径碰撞时换用受控逻辑目录，input hash 不依赖 checkout 根路径。
- PR #277（`657fd93`）将 T06 build-script cache bypass 扩展至 frozen external path package，并识别
  默认 `build.rs` 与 Cargo `[package].build` 自定义脚本；外部自定义脚本被复制进快照并触发 artifact
  cache bypass，普通构建继续可用。该策略不发现脚本的真实文件/环境/网络读集，也不构成完整输入闭包。
- 包含外部 `build.rs` 的 package 在该 root 内任一 watcher 变化时做稳定全量重扫，避免 metadata 复用
  漏过声明范围内的文件变化；这不是对 build.rs 声明或实际读集的发现。目录外读取、环境变量、时间、
  网络等隐藏输入仍不属于该索引闭包。
- filesystem policy 在 macOS/Linux 对已知本地类型启用 metadata 复用，未知/网络/用户态类型回退全量
  内容扫描；Windows 按 volume/drive type 区分固定盘、网络盘和未知盘，并将 UNC 与 verbatim disk path
  分开判定。PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver CI 最终
  全绿；Windows CI 曾暴露 `\\?\D:\...` verbatim disk 被误识别成 UNC，修复后 workspace 429 passed、
  1 个手动 benchmark ignored。
- 原基准数据仍是热文件系统缓存下的 Apple M2/macOS 单机合成集，不能外推到 Linux/Windows 性能、网络/
  用户态挂载、冷缓存或完整 Android/Gradle 输入闭包；外部 Cargo build-script 读集和实挂载对照仍是
  T05 未收口项。原始 JSON 保留在本机 `/tmp`，不作为仓库发布产物。

## T06 Cargo build-script 声明边界证据（2026-10-03）

- PR #279（`2629f82`）将 cache policy 固定为 Cargo `[package].build` 语义：无显式声明时，只有
  package 内存在默认 `build.rs` 才 bypass；`build=false` 不触发；字符串自定义脚本仍触发；workspace
  与冻结 external path package 共用该规则。
- PR #281（`c5003b0`）增加 frozen desktop plan 回归：package 设置 `build=false` 但源码树仍留有
  `build.rs` 时，`build.rs` 会进入冻结快照，`cache_hit_disabled_reason` 保持为空，因而不误关闭 cache
  eligibility。默认/自定义脚本和 external package 的保守 bypass 仍由既有回归覆盖。
- 该边界只表示已声明的 Cargo build-script policy；build.rs 的实际文件/环境/网络读集仍未建模，正常
  冻结和构建可继续，但不能据此宣称完整输入闭包或真实 Android 设备验收。

## T06 编译环境键闭包证据（2026-10-03）

- PR #283（`d132fe9`）将 `RUSTC_WORKSPACE_WRAPPER` 和所选 target 对应的
  `CARGO_TARGET_<TARGET>_LINKER` / `CARGO_TARGET_<TARGET>_RUSTFLAGS` 纳入 BuildKey 环境摘要；target
  triple 按 Cargo 环境变量规则规范化。
- 回归分别改变 workspace wrapper、target linker、target Rust flags，并切换 target triple；每项都会
  改变摘要，且一个 target 的专属变量不会被当成另一个 target 的输入。
- 这只修复已确认的 BuildKey allowlist 遗漏；不发现 wrapper 实际内容、build.rs/Gradle/NDK/Xcode
  任意隐藏 I/O 或远端状态，不能据此宣称完整输入闭包。

## T06 compiler wrapper 内容指纹证据（2026-10-03）

- PR #285（`f911394`）对 `RUSTC_WRAPPER` 与 `RUSTC_WORKSPACE_WRAPPER` 解析到的可执行文件做有界
  内容摘要：读取前后检查 regular-file、大小和修改时间，最多读取 64 MiB，BuildKey 纳入 SHA-256
  与字节数；相同路径替换内容会导致 cache miss。
- wrapper 缺失、非普通文件、解析失败、读取期间变化、超过预算或非 Unicode 环境值会保留构建路径，
  但返回 cache-disabled reason；普通 build/run 仍可执行。desktop/iOS/Android frozen plan 与
  matrix desktop/iOS/Android preview policy 共用该 gate。
- 该保护只覆盖 wrapper 文件本身，不发现 wrapper 启动的子进程、额外文件/环境/网络读取，也不闭合
  build.rs/Gradle/NDK/Xcode 任意隐藏输入。

## T06 native compiler 环境键证据（2026-10-03）

- PR #287（`a385d8a`）将 `AR`、`CFLAGS`、`CXXFLAGS` 纳入显式 BuildKey 环境 allowlist；回归分别
  改变每个变量并确认环境摘要变化。
- 这只覆盖 Cargo/native 编译通过这些变量声明的配置；工具实际读取的其他环境、工具二进制替换、
  build.rs/Gradle/NDK/Xcode 隐式 I/O 与远端状态仍未闭合。

## T06 compiler/linker 工具内容指纹证据（2026-10-04）

- PR #289（`caf03eb`）将 `RUSTC_WRAPPER`、`RUSTC_WORKSPACE_WRAPPER`、`CC`、`CXX`、`AR` 和所选
  target 的 `CARGO_TARGET_<TARGET>_LINKER` 统一解析到可执行文件，并把路径、SHA-256 内容摘要与字节数
  纳入 BuildKey 环境摘要；相同路径替换内容会导致 cache miss。Android matrix preview 按当前 ABI 映射
  Rust target 后使用对应 target linker gate，desktop/iOS 使用 BuildKey 中的 target triple。
- 每次工具读取都有 64 MiB 的共享有界预算，并在读取前后重核验 regular-file、大小、修改时间和 file
  identity；Unix 使用稳定的文件 identity，Windows 使用稳定 Win32 `GetFileInformationByHandle`。
  命令参数、shell 语法、缺失/非普通文件/不可执行文件、读取竞态、非 Unicode 环境值或超预算输入只
  返回 cache-disabled reason，普通构建路径继续。
- 回归覆盖 wrapper、target linker、组合 target linker 以及 `CC`/`CXX`/`AR` 内容替换；不可解析命令、
  带参数或 shell 语法、不可执行文件和超预算工具均确认不会阻止 BuildKey/普通构建，只禁用缓存复用。
  该切片仍不发现工具启动的子进程、额外环境/文件/网络读取，也不闭合 build.rs/Gradle/NDK/Xcode
  任意隐藏 I/O 或远端状态。

## T06 Rust compiler selector 输入证据（2026-10-04）

- PR #291（`5c0cbde`）将 `RUSTUP_TOOLCHAIN` 与 `RUSTC_BOOTSTRAP` 纳入显式 BuildKey 环境摘要，并对
  `RUSTC` 指向的显式 compiler executable 复用同一有界内容指纹；相同路径替换内容会改变摘要。
  `RUSTC` 未设置时仍由已有 `rustc -vV` toolchain identity 表示默认 compiler。
- 选定 `RUSTC` 缺失、非普通文件、不可执行、包含参数或 shell 语法、读取竞态、非 Unicode 环境值或
  超出 64 MiB 预算时只返回 cache-disabled reason，不阻止普通构建；回归确认 `RUSTUP_TOOLCHAIN`/
  `RUSTC_BOOTSTRAP` 变化和显式 compiler 内容变化都会改变环境摘要。
- 该切片只闭合显式 Rust compiler selector 环境，不闭合 Cargo 可执行文件/global Cargo 配置、默认
  `rustc` 之外的工具链隐式读取、编译器子进程或 build.rs/Gradle/NDK/Xcode 隐藏 I/O 与远端状态。

## T06 Cargo flags/profile override 输入证据（2026-10-04）

- PR #293（`b2dc719`）将 `CARGO_BUILD_RUSTFLAGS` 与 dev/release profile 的输出相关环境覆盖纳入
  BuildKey 环境摘要：debug、debug assertions、codegen units、incremental、LTO、opt-level、
  overflow-checks、panic、rpath、split-debuginfo 和 strip。
- 回归逐项改变上述变量，并确认每项都会改变环境摘要；普通构建路径不因这些环境值缺失或变化而被
  阻止，缓存复用只在 BuildKey 不同后自然失效。
- 该切片只覆盖当前 CLI 使用的 dev/release profile 显式环境覆盖，不闭合 custom profile 覆盖、Cargo
  可执行文件/global Cargo 配置、build.rs/Gradle/NDK/Xcode 隐藏 I/O 或远端状态。

## T06 Android global Gradle 配置 cache bypass（2026-10-04）

- PR #295（`dbb275d`）解析显式 `GRADLE_USER_HOME`，否则按平台使用默认 user home；存在
  `gradle.properties`、`init.gradle`、`init.gradle.kts` 或 `init.d` 时禁用 Android artifact cache reuse。
- `GRADLE_HOME`、`GRADLE_OPTS`、`JAVA_OPTS`、`JAVACMD`、`JAVA_TOOL_OPTIONS`、`JDK_JAVA_OPTIONS`、
  `_JAVA_OPTIONS` 或任意 `ORG_GRADLE_PROJECT_*` 环境注入同样触发 bypass；home 未知/不可解析时 fail closed。
  诊断只给固定原因，不读取或输出配置路径、属性名或原文；普通 Android build/preview 仍可继续。
- PR #296（`8e44e44`）让 cache-disabled 的普通 Android build 与 live preview 不发布新的 verified artifact
  manifest；普通 build 直接返回存在且可用的 APK。回归确认 bypass 不覆盖旧 manifest，旧 hash 在输出变化后
  验证失败，从而避免配置移除后命中 bypass 构建产物。
- #295 先按 build plan 检查已知用户级入口，#298 检查 wrapper distribution 的 `init.d`，#300 再指纹化标准
  wrapper layout 下的 distribution 内容；这些切片都不冻结或在 build 前后重核验 user home，因此不闭合
  plan 后并发修改、任意 Gradle/plugin/build-script I/O 或远端仓库状态。

## T06 Gradle wrapper distribution init scripts（2026-10-04）

- PR #298（`a10dd0c`）在 user-home cache gate 中检查 `wrapper/dists/<distribution>/<hash>/<gradle-root>/init.d`，
  覆盖所有已安装 distribution，而不只项目当前版本。目录遍历共享最多 4096 个 entry 的预算；仅普通
  `readme.txt` 放行，其他 entry、符号链接、不可读状态或超预算只关闭 cache reuse。
- 不读取脚本原文或发行版其他文件内容，不记录绝对路径；普通 Android build/preview 继续，只是不消费/发布
  可复用 manifest。本机实际 Gradle 9.4.1/9.7.0 解压目录只有官方 `readme.txt`，cache-hit smoke 仍通过。
- 这只覆盖 Gradle installation init scripts，不 fingerprint 整个解压 distribution，也不冻结 wrapper home；
  plan 后的配置变化、任意 Gradle 插件/build-script I/O 和远端仓库状态仍未闭合。

## T06 Gradle wrapper distribution content fingerprint（2026-10-04）

- PR #300（`b0767ab`）将标准 `distributionBase=GRADLE_USER_HOME` / `distributionPath=wrapper/dists` 下
  所有已安装 distribution 的相对路径、目录项和文件内容摘要纳入 Android toolchain identity/BuildKey；
  安装目录旁的普通状态/ZIP 文件也纳入，瞬态 `.lck` 文件排除。扫描包括非活动版本，路径摘要不含 user-home
  绝对路径。
- wrapper properties 缺少固定 checksum、使用非标准/重复/escaped 安装布局，或当前尚无已安装 distribution 时，
  cache reuse bypass；symlink、special entry、读取/编码失败或超过 100,000 entries / 512 MiB 预算也只
  bypass cache reuse，普通构建继续。distribution 在该次构建中下载后不为该次 plan 发布可复用 manifest。
- 本地 fingerprint 使用有界完整内容扫描，每次 Android BuildKey plan 增加文件读取成本；尚无跨平台性能对照。
  该摘要不冻结 user home，也不在 Gradle 执行前后重核验，因而不解决 plan 后并发变化。

## T06 Gradle distribution identity revalidation（2026-10-04）

- PR #302（`1c8cbd5`）将 distribution identity（当前绝对 `GRADLE_USER_HOME`、内容 fingerprint）随 Android
  build plan 与 preview policy 进入执行路径。普通 build 在 cache reuse 前、Gradle 完成后及 manifest 发布前
  重核验；已知 `gradle.properties`/init/env gate 也参与当前状态检查。变化时使用独占普通构建，APK 仍返回，
  但不消费或发布旧 BuildKey 的 reusable manifest。
- matrix/live preview 使用与 Android build key 相同的 policy key；cache hit 前和 Gradle 后重核验。构建期间
  distribution 变化时，preview coordinator 允许已有 APK 作为 unshared 结果完成，但不把它当作 verified
  reusable artifact。相对 `GRADLE_USER_HOME` 的 preview 直接 bypass，避免 snapshot root 与 Gradle cwd 解析分叉。
- 回归覆盖：配置出现/消失和 distribution content 改变、policy/build-key 一致性、unshared APK 不发布 manifest、
  当前环境路径与 fixture fingerprint 分离；本地每次 Android plan 仍有完整有界扫描的 I/O 成本。

## T06 Local Gradle build logic cache gate（2026-10-04）

- PR #304（`ccdecf5`）对 `mobile/android/gradle/buildSrc` 与 `build-logic` 下的任何已扫描文件启用保守
  cache bypass。该类 convention/plugin code 可能在 Gradle configuration/build 阶段读取环境、用户文件、网络或
  改写 variant/signing，当前 CLI 不解析其实际读集，因此不消费/发布 artifact cache；普通 build/preview 继续。
- gate 只检查路径是否属于这两个本地 build-logic 根，不读取 plugin 内容、不把路径或内容写入 BuildKey；普通
  `app/src/main` source 不触发该 gate。动态 dependency/signing/included-build checks 仍独立运行。
- 这只缩小了已知本地 plugin 输入面；普通 `build.gradle(.kts)`、AGP/plugin implementation 的任意 I/O、远端
  repository metadata/state 和真实设备验收仍未闭合。

## T06 Ordinary Gradle app-script I/O cache bypass（2026-10-04）

- PR #306（`16083d0`）对 `mobile/android/gradle/` 下非 `buildSrc`/`build-logic` 的 `.gradle` 与
  `.gradle.kts` 脚本增加保守静态 marker gate。未建模的 Gradle provider/environment 读取、文件读取、
  网络/进程访问、`apply from` 和 `includeBuild` marker 会关闭 artifact cache reuse；普通 build/preview
  仍继续执行。
- gate 不读取或指纹化脚本引用的外部内容，也不把脚本路径或 marker 写入 BuildKey。已建模的
  `gpui.abis`/`gpui.buildDir`/`gpui.jniLibsDir`、`GPUI_ANDROID_ABIS`/`ANDROID_NDK_HOME`、NDK
  `source.properties` 和受控 `keystore.properties` 读取保持可复用；注释、字符串和普通
  `app/src/main` source 不触发该 gate。
- 该切片是有界静态扫描，不是 Gradle DSL 解析器或运行时读集追踪；未识别的 plugin/script I/O、远端
  repository metadata/state、复杂 signing 和真实设备验收仍保持未闭合。

## T06 Unsigned Android release artifact cache hit（2026-10-04）

- PR #308（`9d9cad5`）允许没有 signing 配置、没有敏感 signing 输入的 Android release variant
  复用非 live BuildKey artifact manifest。该路径只产生 Gradle 的 `*-unsigned.apk`，不把任何 keystore
  或远端 signing 状态假装纳入 BuildKey；release preview 仍显式 bypass。
- `plugins {}` 中未知 plugin、version-catalog alias、`apply(plugin = ...)` 或 plugin-owned signing
  behavior 会保守关闭 cache reuse；已知 `com.android.application`/`com.android.library` 保持模板可复用。
  custom/remote signing、敏感输入和未识别运行时 I/O 继续 bypass。
- 真实 smoke 现在分别执行 debug 与 `--release` 两次 CLI build：两种 variant 第一次均为 cache miss，
  第二次均为 verified BuildKey cache hit，并检查 unsigned release APK 的两个 ABI。

## T06 Custom Gradle repository cache gate（2026-10-04）

- PR #310（`bbcf21e`）对 Android Gradle `repositories {}` 做保守静态扫描：仅允许
  `google()`、`mavenCentral()`、`gradlePluginPortal()`，自定义 `maven {}`、`mavenLocal()`、
  `flatDir {}`、`exclusiveContent {}` 和其他 repository entry 只关闭 artifact cache reuse，普通
  build/preview 继续。
- gate 不读取或 fingerprint 标准远端 repository runtime/state，也不把 repository URL 写入 BuildKey；
  因此标准仓库状态、未识别 repository/plugin I/O、复杂/远端 signing 和真实设备验收仍未闭合。
- 本机真实 smoke 的模板标准 repositories 保持 debug 与 unsigned release 两种 miss→hit；自定义
  repository 仅由静态回归覆盖为 cache miss。

## T06 Gradle buildscript classpath plugin cache gate（2026-10-04）

- PR #312（`d5abe82`）对 Android Gradle `buildscript` 中的 `classpath` 做保守静态扫描：只有字面量、固定
  版本的 `com.android.tools.build:gradle:<version>` 坐标保持 cache eligibility；未知坐标、动态版本、
  version-catalog 或非字面量 classpath 只关闭 artifact cache reuse，普通 build/preview 继续。
- 该 gate 复用既有未知 plugin cache-disabled reason，不读取 plugin 实现内容，也不把 classpath 坐标额外写入
  BuildKey；固定 AGP 坐标由现有 verification metadata、wrapper 和 dependency policy 继续约束。
- 回归覆盖模板已知 AGP classpath、未知 plugin 坐标、version-catalog classpath 和版本变量；真实 Android
  smoke 继续验证模板 debug/release packaging 与 CLI debug/release miss→hit，说明已知模板 classpath 未被误判。

## T06 Gradle provider/project file I/O cache gate（2026-10-04）

- PR #314（`9584673`）扩展 Android Gradle app-script 的保守 I/O marker，覆盖
  `providers.fileContents`、`projectDirectory`/`projectDir`/`rootDir`、`gradleLocalProperties`、
  `fromArchiveEntry` 等 archive-entry provider，以及 `asFile`/`getAsFile`/`getAsFileTree` 等 provider
  file materialization；命中时只关闭 artifact cache reuse，普通 build/preview 继续。
- 这些 marker 复用既有 app-script I/O cache-disabled reason，不把外部文件内容、路径或 provider 值写入
  BuildKey；模板的已建模 GPUI/NDK/签名读取仍保持可复用，未知 marker 继续保守 bypass。
- 回归覆盖上述入口，并确认注释/字符串和普通 app source 的既有排除边界；真实 Android smoke 继续验证
  debug/release packaging 与 CLI miss→hit，说明模板没有被新增 marker 误判。

## T06 Gradle custom provider/ValueSource cache gate（2026-10-04）

- PR #316（`a248be9`）对 Android Gradle app script 的 `ProviderFactory` custom provider/`ValueSource` 入口
  增加保守静态 marker：`providers.of`、`providers.provider` 和 `ValueSource` 只关闭 artifact cache reuse，
  普通 build/preview 继续。
- gate 不解析 ValueSource 实现或 provider closure 的实际读集，也不把 provider 内容或路径写入 BuildKey；
  它只扩大 cache miss 范围，避免未知闭包/自定义 provider 在未建模输入下复用旧产物。
- 回归覆盖 custom provider、provider closure 和 ValueSource 声明；真实 Android smoke 继续验证模板
  debug/release packaging 与 CLI debug/release miss→hit，说明模板未被新增 marker 误判。

## T06 Gradle `add("classpath", ...)` plugin cache gate（2026-10-04）

- PR #318（`c754013`）对 Android Gradle `buildscript` 中的 `add("classpath", ...)` 动态依赖写法复用
  未知 plugin cache-disabled reason。只有字面量、固定版本的
  `com.android.tools.build:gradle:<version>` 坐标保持 cache eligibility；未知坐标和非字面量/catalog
  值只关闭 artifact cache reuse，普通 build/preview 继续。
- 回归覆盖固定 AGP 放行、未知 plugin 坐标和 `libs.plugins...` 非字面量值，并保留直接 `classpath(...)`
  的既有覆盖；不读取 plugin 实现内容或将坐标额外写入 BuildKey。
- 该切片不闭合 plugin 实现的任意运行时 I/O、标准远端 repository runtime/state、复杂/远端 signing、
  release preview 或真实设备验收。

## T06 Gradle file/files path-resolution cache gate（2026-10-04）

- PR #320（`822030c`）扩展 Android Gradle app-script I/O marker，识别通用 `file(...)` 和 `files(...)`
  路径解析调用；未建模调用只关闭 artifact cache reuse，普通 build/preview 继续。
- 模板的两类显式路径例外保持 cache eligible：由 `gpui.buildDir` provider 值解析的 GPUI build directory，
  以及在 `ANDROID_NDK_HOME`/`source.properties` 上下文中解析的 NDK 路径。单测还验证同脚本额外加入未知
  `file("config.json")` 时仍 bypass，避免已建模路径放行整个脚本。
- 该 marker 不追踪解析后对象的后续使用或任意 Gradle runtime I/O；标准远端 repository runtime/state、
  复杂/远端 signing、release preview 和真实设备验收仍未闭合。

## T06 Gradle source-root declaration cache gate（2026-10-04）

- PR #322（`dfc023c`）扩展 Android Gradle app-script I/O marker，识别 `srcDir(...)` 和 `srcDirs(...)`
  source-root 声明；未建模调用只关闭 artifact cache reuse，普通 build/preview 继续。
- 模板例外只允许 `sourceSets`/`jniLibs` 上下文下以 `gpuiJniLibsDir` 为参数的 `srcDirs(...)`，并要求脚本
  显式读取受控 `gpui.jniLibsDir` Gradle property。其它 Java/Kotlin/resource/generated source roots 保守 bypass。
- 回归确认模板 JNI root 放行、任意 Java `srcDir`/`srcDirs` bypass；真实 Android 模板 debug/release packaging
  与 CLI miss→hit smoke 通过。该静态 marker 不解析 Gradle source provider/runtime 文件集合；标准远端 repository
  runtime/state、复杂 signing、release preview 和真实设备验收仍未闭合。

## T06 Gradle source-root DSL alias cache gate（2026-10-04）

- PR #325（`1b765ee`）补齐 source-root 声明的替代 DSL 形式：`setSrcDirs(...)`、`srcDirs = ...`、Groovy
  command-style `srcDir 'path'` 和 `srcDirs += ...` 现在只关闭 Android artifact cache reuse，普通构建继续。
- 模板 `srcDirs(gpuiJniLibsDir)` 受控例外保持可复用；回归验证注释和字符串中的 marker 文本不会误触发。
- 该 gate 仍是保守静态扫描，不追踪 source provider 的运行时目录/文件变化，也不构成完整 Gradle 输入闭包或设备验收。

## T06 Gradle command-style file path cache gate（2026-10-05）

- PR #327（`9c0fef8`）补齐 Groovy command-style `file 'path'` / `files 'path'` 路径解析，以及
  `srcDirs files 'path'` source-root 声明；未建模输入只关闭 Android artifact cache reuse，普通构建继续。
- 回归确认注释和字符串中的类似文本不触发 marker；真实模板 debug/release packaging、两个 ABI 和 CLI
  debug/release miss→hit cache smoke 通过。
- 该 gate 仍是保守静态扫描，不追踪返回对象后续使用、source provider/runtime 文件变化或完整 Gradle 输入闭包，
  也不等于真实设备、复杂 signing、release preview 或标准远端 repository runtime/state 验收。

## T06 Gradle Java/Kotlin path constructor cache gate（2026-10-05）

- PR #329（`cc1da49`）将 `File(...)`、`java.nio.file.Paths.get(...)`、`java.nio.file.Path.of(...)` 纳入
  Android app-script I/O marker；覆盖限定与非限定类名，未建模调用只关闭 artifact cache reuse，普通构建继续。
- 回归验证注释和字符串中的相似表达式不触发；Android debug/release packaging、ABI 与 CLI debug/release
  miss→hit smoke 通过。
- 该 marker 不追踪构造对象后续的读取或完整 Gradle runtime I/O，也不构成远端 repository、复杂 signing、
  release preview 或真实设备验收。workspace 全量测试的一次并行运行中 4 个既有进程时序测试失败，逐项单线程
  重跑通过；CI 两套 workflow 全绿。

## T06 Gradle class/resource lookup cache gate（2026-10-05）

- PR #331（`8a4d4b4`）扩展 Android Gradle app-script I/O marker，覆盖 `ClassLoader` resource lookup、
  `ServiceLoader` 与 `Class.forName`；命中时只关闭 artifact cache reuse，普通构建继续。
- 回归验证相应入口触发 bypass，而注释和字符串不触发；真实 Android debug/release packaging、ABI 与 CLI
  miss→hit smoke 通过。
- 该静态 marker 不追踪加载类后的任意 I/O、plugin/runtime 行为或完整 Gradle 输入闭包；复杂/远端 signing、
  标准远端 repository runtime/state、release preview 和真实设备验收仍未闭合。

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
- cache-hit 子流程用临时 `GRADLE_USER_HOME`；只将原 user home 的 `caches` 与 `wrapper` 目录链接进来，
  不带入根目录 `gradle.properties`/`init.gradle(.kts)`/`init.d`，并清理显式 Gradle/JVM 注入变量，使 smoke
  验证的是无未建模 global config 时的真实 miss→hit 路径；wrapper 链接保留 distribution，cache gate 仍会
  检查该 distribution 内部的 `init.d`。
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
- 模板携带 `gradle/verification-metadata.xml`，按 Gradle 9.4.1 的真实 debug/release 任务图生成，
  为组件及其解析制品固定 SHA-256（346 components / 607 artifacts），并覆盖 AGP 在 macOS/Linux/Windows
  上按需选择的 AAPT2 classifier。Gradle 自身负责下载/使用制品时逐字节验证；CLI 只在 XML 有效、
  `verify-metadata=true`、每个 component 至少一个 artifact 且每个 artifact 恰有一个 64 位 SHA-256、
  没有 `trusted-artifacts` 规则时允许缓存命中；metadata 文件自身也进入 BuildKey 输入摘要。metadata
  缺失、畸形或宽松时不阻止 Gradle 构建，只禁用 artifact cache reuse。
- Android toolchain identity 通过 `java -XshowSettings:properties -version` 获取实际 `java.home` 与
  `java.version`，对该 JDK 做有界内容摘要。JDK 内部目录链接按逻辑路径递归，外部目录链接也按
  逻辑路径递归，外部文件链接只记录类别和文件内容 hash；绝对安装路径不进入 fingerprint。循环、
  断链、特殊文件、不可读内容或超过 100,000 entries/512 MiB 时 cache reuse bypass，普通构建继续。

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
- PR #267 首轮 Android 构建在 `compileDebugNavigationResources` 失败，报告指出 AGP 动态解析的
  host AAPT2 classifier 不在 help/configuration 阶段生成的 metadata 中；补入 Linux/macOS/Windows
  Google Maven JAR 的独立 SHA-256 后，Gradle 从真实 debug/release 任务图重生成的 2,534 行 metadata
  与模板文件逐字节一致。验证清单固定 Gradle 9.1.0 AGP graph 中 346 个 component / 607 个 artifact；
  Gradle `verify-signatures` 保持 false，只声明 SHA-256 内容完整性，不宣称 Maven 签名验证。407 个
  workspace 单测及全部集成/协议测试、clippy、fmt、design docs、
  package list、diff check 通过；本地 Android debug/release APK、ABI 检查和 CLI 二次 cache hit 通过；
  最终 PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver CI 全绿；
  #267 squash 为 `bdfd717`，无版本发布或 tag。
- PR #269 前两轮 Android probe 暴露 Temurin JDK 的内部/外部 truststore 与目录链接布局；最终按
  逻辑树递归处理外部目录、仅 hash 外部文件且不泄露绝对路径，保留断链/循环/特殊文件拒绝。411 个
  workspace 单测及全部集成/协议测试、clippy、fmt、design docs、package list、diff check 通过；
  本地 Android debug/release、ABI 检查和 CLI 二次 cache hit 通过；最终 PR 与 push 两套
  Linux/macOS/Windows、Android/desktop template、baseline-driver CI 全绿；#269 squash 为
  `4aa6ed3`，无版本发布或 tag。
- PR #295 的本机 Gradle global-config gate 与 user-home 路径回归通过；workspace 445 passed、1 个
  手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过。本机
  Android debug/release APK 与 CLI miss→hit smoke 通过；PR 与 push 两套 Linux/macOS/Windows、
  Android/desktop template、baseline-driver 全绿；squash 为 `dbb275d`，无版本发布或 tag。
- PR #296 的 Android bypass manifest 发布/不覆盖回归通过；workspace 445 passed、1 个手动 benchmark
  ignored，clippy/build/Windows target check/fmt/design docs/package list 通过。本机 Android debug/release
  APK 与 CLI miss→hit smoke 通过；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、
  baseline-driver 全绿；squash 为 `8e44e44`，无版本发布或 tag。
- PR #298 的 wrapper distribution init-script presence、official README allowlist、custom-script bypass 回归通过；
  workspace 446 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/package list 通过。
  本机 Android debug/release APK 与 CLI miss→hit smoke 通过；PR 与 push 两套 Linux/macOS/Windows、
  Android/desktop template、baseline-driver 全绿；squash 为 `a10dd0c`，无版本发布或 tag。
- PR #300 的 installed distribution contents/marker 替换、inactive distribution、标准路径策略、escaped/nonstandard
  layout bypass、超预算与 symlink fail-closed 回归通过；workspace 448 passed、1 个手动 benchmark ignored，
  focused Gradle/toolchain tests、clippy/build/Windows target check/fmt/design docs/package list 通过。Pinned
  Gradle wrapper probe、本机 Android debug/release APK 与 CLI miss→hit smoke 通过；PR 与 push 两套 CI 全绿，
  squash 为 `b0767ab`，无版本发布或 tag。
- PR #302 的 distribution/global identity 复用前/后重核验、relative user-home preview bypass、policy/build-key
  对齐和变化期间 unshared APK 保留/manifest 抑制回归通过；workspace 449 passed、1 个手动 benchmark ignored，
  clippy/build/Windows target check/fmt/design docs/package list 通过。本机 Android debug/release APK 与 CLI
  miss→hit smoke 通过；PR 与 push 两套 CI 全绿，squash 为 `1c8cbd5`，无版本发布或 tag。
- PR #304 的 buildSrc/build-logic 检测与普通 app source 排除回归通过；workspace 451 passed、1 个手动 benchmark
  ignored，focused gate test、clippy/build/Windows target check/fmt/design docs/package list 通过。本机 Android
  debug/release APK 与 CLI miss→hit smoke 通过；PR 与 push 两套 CI 全绿（push macOS failed job rerun 后通过），
  squash 为 `ccdecf5`，无版本发布或 tag。
- PR #306 的 ordinary app Gradle I/O marker、已建模 GPUI/NDK/签名读取、注释/字符串与普通 app source 排除
  回归通过；workspace 451 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design
  docs/package list 通过。本机 Android debug/release APK、ABI 检查与 CLI miss→hit smoke 通过；PR 与 push
  两套 CI 全绿，squash 为 `16083d0`，无版本发布或 tag。
- PR #308 的 unsigned release cache eligibility、release preview bypass、未知 plugin/alias/plugin-owned
  signing marker 回归通过；workspace 454 passed、1 个手动 benchmark ignored，clippy/build/Windows target
  check/fmt/design docs/package list 通过。本机 Android debug 与 unsigned release APK/ABI 检查、CLI 两种
  variant miss→hit smoke 通过；PR 与 push 两套 CI 全绿，squash 为 `9d9cad5`，无版本发布或 tag。
- PR #310 的标准 repository allowlist 与 custom repository bypass 回归通过；workspace 455 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug 与 unsigned release APK/ABI 检查、CLI miss→hit smoke 通过；PR 与 push 两套 CI 全绿，squash 为 `bbcf21e`，无版本发布或 tag。
- PR #312 的已知 AGP buildscript classpath 保持 cache eligibility、未知/动态/version-catalog/非字面量 classpath bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含 arm64-v8a/x86_64；PR 与 push 两套 CI 全绿，squash 为 `d5abe82`，无版本发布或 tag。
- PR #314 的 provider/project file I/O marker bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含 arm64-v8a/x86_64；PR 与 push 两套 CI 全绿，squash 为 `9584673`，无版本发布或 tag。
- PR #316 的 custom provider/ValueSource marker bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含 arm64-v8a/x86_64；PR 与 push 两套 CI 全绿，squash 为 `a248be9`，无版本发布或 tag。
- PR #318 的 `add("classpath", ...)` 固定 AGP 放行、未知坐标和非字面量/catalog bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging、ABI 检查与 CLI miss→hit smoke 通过，缓存 APK 含 arm64-v8a/x86_64；PR 与 push 两套 CI 全绿，squash 为 `c754013`，无版本发布或 tag。
- PR #320 的 `file(...)`/`files(...)` 未建模路径 bypass、GPUI build-dir/NDK path 例外与混合未知输入回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging、ABI 检查与 CLI miss→hit smoke 通过，缓存 APK 含 arm64-v8a/x86_64；PR 与 push 两套 CI 全绿，squash 为 `822030c`，无版本发布或 tag。
- PR #322 的未建模 `srcDir(...)`/`srcDirs(...)` bypass 与模板 JNI `gpui.jniLibsDir` 例外回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging、ABI 检查与 CLI miss→hit smoke 通过，缓存 APK 含 arm64-v8a/x86_64；PR 与 push 两套 CI 全绿，squash 为 `dfc023c`，无版本发布或 tag。
- 文档 PR #323 将审计基准更新至 `1475d24`，补齐 #322 的 source-root gate、模板 JNI 例外、本地验证和未闭合边界；文档检查、PR/push 两套 CI 全绿，squash 为 `1475d24`，无版本发布或 tag。
- PR #325 的 `setSrcDirs(...)`、属性赋值、Groovy command-style source-root bypass、模板 JNI 例外与注释/字符串排除回归通过；workspace 456 passed、1 个手动 benchmark ignored，clippy/build/fmt/design docs/package list、diff check 通过。PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 最终全绿；PR run 首次 macOS 遇到既有 registry manifest 测试偶发失败，按 job 重跑通过，fail-fast 取消的 Linux/Windows jobs 也分别重跑通过；squash 为 `1b765ee`，无版本发布或 tag。
- PR #327 的 Groovy command-style `file/files`、`srcDirs files` bypass 与注释/字符串排除回归通过；workspace 456 passed、1 个手动 benchmark ignored，clippy/build/fmt/design docs/package list、diff check 通过；真实 Android debug/release packaging、`arm64-v8a`/`x86_64` ABI 与 CLI debug/release miss→hit smoke 通过；PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿；squash 为 `9c0fef8`，无版本发布或 tag。
- PR #329 的 `File(...)`、`Paths.get(...)`、`Path.of(...)` 限定/非限定路径 marker 与注释/字符串排除回归通过；clippy/build/fmt/design docs、package list、diff check 和真实 Android debug/release packaging、ABI、CLI miss→hit smoke 通过。并行 workspace test 首次 452 passed、1 ignored、4 个现有 coordinator/devserver 时序测试失败，四项单线程单独重跑通过。PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿；squash 为 `cc1da49`，无版本发布或 tag。
- PR #331 的 `ClassLoader` resource APIs、`ServiceLoader.load`、`Class.forName` marker 与注释/字符串排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs、Android debug/release packaging、ABI 与 CLI miss→hit smoke 通过；PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿；squash 为 `8a4d4b4`，无版本发布或 tag。

## 未覆盖

- 最小 app 主动移除了模板的 GPUI/gpui-mobile 依赖，因而不验证 GPUI renderer、窗口初始化、
  JNI 行为或完整 app 冷启动；它只验证 `gpui build android` 的冻结输入、真实 NDK 编译、
  Gradle APK 打包、manifest 校验和第二次 cache hit；
- Android emulator/device 安装启动、custom/remote signing、release preview、cache 并发订阅/取消引用和容量
  清理仍未覆盖。
补充：#295/#298/#300/#302/#304/#306/#308/#310/#312/#314/#316/#318/#320/#322 的 gate 在 build plan 检查已知 user-home 配置/环境入口、`init.d`，对标准 wrapper
  layout 中已安装 distribution 内容做有界 fingerprint，在复用前/Gradle 后重核验当前 identity，并对本地
  buildSrc/build-logic 直接 bypass；#306 又对普通 Gradle app script 的一组已知 I/O marker 直接 bypass；#308
  对 unsigned release 允许非 live cache hit，并对未知 plugin signing behavior 直接 bypass；#310 对自定义
  repository entry 直接 bypass cache reuse，但不读取或 fingerprint 标准远端 repository runtime/state；#312 对
  未知 buildscript classpath、#318 对 `add("classpath", ...)` 的未知/非字面量值、#320 对未建模 `file(...)`/`files(...)` 路径解析、#322 对未建模 `srcDir(...)`/`srcDirs(...)` source roots、已知 provider/project file I/O marker 和 custom provider/ValueSource marker 直接 bypass cache reuse；它不锁定或复制
  user home，检查后的并发修改仍可能竞态。非标准 wrapper layout 和相对 preview user home 只 bypass；未识别
  环境变量、未被 marker 识别的 app build-script/plugin I/O、custom/remote signing 仍未建模。
- cache-hit CI 子流程主动排除了 root user-home 配置，因此它验证干净配置下的 cache hit；存在上述 global
  配置时预期是普通构建成功但 cache reuse bypass，不把该情形算作设备或完整 Gradle 输入验收。
- wrapper checksum 校验 Gradle distribution ZIP；#300 另对当前已安装 wrapper distributions 做有界内容摘要，
  #302 在执行前后重核验该摘要并抑制变化后的 reusable manifest，但不覆盖 AGP/plugins 的任意运行时 I/O、远端
  仓库状态、未被 #306/#308 marker 识别的普通 app build-script/AGP plugin I/O、custom/remote signing、
  NDK/build-script I/O，也不替代完整 Android 构建输入闭包或设备验收。
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
- PR #267 的 verification metadata 锁定模板 Gradle 9.1.0 AGP dependency graph 和验证时观察到的
  debug/release artifacts；升级 Gradle、AGP 或任务图后必须重新生成并复核元数据。静态列表不锁定远端
  仓库可用性、仓库状态、构建脚本任意网络 I/O 或未被任务图触发的可选制品，也不验证 Maven 签名；Gradle
  verification metadata 当前使用 SHA-256 内容校验且 `verify-signatures=false`。用户修改依赖或版本但
  未更新校验元数据时构建会由 Gradle 拒绝，不能靠 cache bypass 静默接收新制品。
- PR #269 的 Java runtime fingerprint 只描述实际 `java.home` 运行时树的逻辑内容，不锁定安装根路径；
  外部 truststore 文件变化会改变摘要，外部目录内容也会被递归纳入。它不覆盖 Gradle daemon 外的
  其他 JVM、远端仓库状态或任意 build-script I/O；JDK 的特殊布局变化可能安全地退化为 cache bypass。
