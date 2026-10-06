# 主分支进度与接续记录

更新日期：2026-10-07（Asia/Shanghai）。合并代码审计基线仍为 `852ddec`（PR #377 squash merge）；
流程记录已合并至 `757f05b`（PR #383 squash merge），其中包含 T01 doctor、F01 baseline-driver、
Ubuntu `cc --version` 解析修复、iOS-safe backtrace 模板修复及 F01/P-01 native-install-failure
证据、T01 doctor CLI host-smoke 和 target-selection。当前分支为 `main`，工作区干净；PR #382
已以 squash 合并为 `9e6ef64`，PR #383 已以 squash 合并为 `757f05b`，两套三平台
workspace/template/baseline-driver CI 全绿。T01/F01/P01 父状态未晋升。

本文接续 2026-09-28 对 `a6aa685` 的审计，替代其“当前进度”结论；旧报告保留为历史证据。
任务状态以[实施清单](implementation-backlog.md)为准，完成标准以[验收矩阵](acceptance-matrix.md)为准。
专项设计中的目标接口和历史实验记录不能直接当作当前实现或全平台验收。

执行入口改为[收口计划](closeout-plan.md)：默认持续推进整体核心路线，每次按依赖和可用
验收环境选择一个收口焦点，完成后自动选择下一项，不绑定固定任务 ID。原有 35 项状态计数不变；不要从最近 Gradle cache PR 继续
自动选择加固切片。共享验收按[矩阵](acceptance-matrix.md)第 1.4 节登记责任和联合出口。

## 1. 当前结论

基础 CLI/Live 已有实现，macOS 窗口观察有限可用；场景、输入、check、视觉基线和本地 matrix
已接入代码。单场景 desktop check 和 matrix check 均已接入严格冻结输入路径；matrix admission
前创建的 workspace snapshot 由所有 cell 复用，per-cell runtime context 也已进入 MatrixReport。
iOS simulator/Android runner、移动 matrix control driver 也已落地；Android capture artifact 现在
会 best-effort 记录逻辑 viewport、density-derived scale、方向和保守的前台包名归属，探测失败时
保持 unknown；preview desktop/iOS/Android
构建现在会在 source-project 的 target-specific output root 上取得跨进程锁，串行保护同一输出根的
输出变更；desktop preview、iOS simulator live preview、Android default-debug 与显式 debug
custom-signing live preview 已能基于独立 verified artifact manifest 命中并跳过相应构建步骤；
matrix control scenario cell 现在保留完整 `CheckReport`，包括 steps、证据和 cleanup；移动 preview
子进程与 matrix supervisor 共用同一 delegated device lease，capture/native-log/stop evidence
绑定 preview 实际 run，并写入 `CheckContext.mobile_evidence`；pre-`scenario_ready` 的移动
launch/registration failure 也会保留可取得的 native-log/stop/lease evidence，并以
`run_id_bound=false` 标记尚未绑定实际 preview run；action operation 出错且错误详情含
operation ID 时，scenario action evidence 也保留状态和 ID，unknown 仍为 inconclusive 且不重放；
cleanup 的 stop/release 错误也写入 mobile evidence，stop 失败时仍释放 lease。
matrix executor 在串行和并行 cleanup 完成后重新读取 runner context，因此 cleanup 阶段才生成的
stop、lease release 和 cleanup error evidence 也会进入 `MatrixReport`；capture-only mobile
lifecycle cell 会保留 launch/capture/native-log/stop/release evidence，但仍不生成伪造的
scenario `CheckReport`。
capture-only matrix 与 control-driven scenario 两条移动路径现在也把 `RunnerInfo` 和
`RunnerCapabilities` 写入 `mobile_evidence`，将报告绑定到选定 runner/device 的声明性环境；这
不是实际设备探针、前台身份或完整语义/输入验收。
PR #253 收紧本地 matrix admission：iOS/Android 的静态默认能力不再包含 `semantics`、
`semantics.read` 或 `semantics.bounds`，因此依赖移动语义的 cell 在没有显式 runner capability
override 时于派发前标记 unavailable；capture/reset/input 默认能力不变。override 只是调用方声明，
不由 admission 自动验证设备证据。
这仅修正 admission 声明，不实现移动 semantics provider，也不修改 runtime 的 hello/capability
状态或 GPUI accessibility 激活条件；真实语义树及移动设备验收仍未完成。
平台 adapter 维护的有界 `EvidenceLog` 现在也随两条移动路径进入 `mobile_evidence`，保留
install/launch/capture/process/channel/native-log/stop 的事件序列；每个 run 最多保留 128 条事件，
单条 `details` 的序列化结果最多 16 KiB，超限时保留摘要/hash，并通过 `truncated` 与
`dropped_events` 暴露诊断；事件淘汰保留最近记录，JSON 反序列化也重新执行同一边界。事件证据
已接线；每条 `EvidenceEvent` 也重复写入 `project_id`、`lease_session_id` 和
`fencing_token_sha256`，因此单条事件脱离外层聚合仍可核对 run/project/device/lease/fencing 摘要；
capture-only matrix context 现在同时传播 capture provider、artifact hash、逻辑 viewport、
scale、方向、系统 UI 和保守前台包名等环境字段，并保留 prepare 阶段生成的
`run_identity`（run/project/device、lease session 和 fencing token 摘要）；原始 fencing token
不进入 JSON，未建立身份时显式写入 `run_id_bound=false`。真实设备 fault matrix、重连和连续验收
仍未完成。control-driven mobile scenario evidence 也补齐 `project_id`，并仅在实际 preview
run 已绑定后发布嵌套 `run_identity`；`scenario_ready` 之前保持 `run_identity=null`。
移动 runner 的 capture event 现在也保留已取得的 artifact/hash、像素与逻辑 viewport、scale、方向、
系统 UI 和前台 marker；host path 与 lease secret 不进入事件详情，缺失探针继续保持 null/unknown。
capture-only matrix 和 control-driven check 在发布移动截图证据前会重新验证文件类型、大小、字节数、
SHA-256、PNG 尺寸和 `png-<sha256>` artifact ID；输出被替换或损坏时拒绝该证据，不把完整性失败
降格为可用截图。每张移动截图还会在同目录原子发布 schema 1 的
`<capture>.manifest.json`，记录 artifact/hash/bytes/PNG 尺寸、逻辑 viewport/scale/方向、系统 UI、
前台 marker 及 run/project/device/lease/fencing digest；Android display metadata 补齐后会重新发布
manifest。check 与 matrix 发布前会同时核对 manifest 和当前 `RunIdentity`，报告只输出相对
`manifest_path`；原始 fencing token 和绝对宿主路径不进入 manifest/report。该切片仍不等于真实设备
fault matrix、重连或连续验收。PR #245 又将该相对路径暴露到 `ScreenshotEvidence` 及 screenshot
assertion evidence，并拒绝 artifact root 外的绝对路径或越界路径。
PR #247 又将 cell/target/scenario、requirements 和 frozen fixture hash 绑定到 mobile lifecycle
evidence，使 pre-ready、cleanup failure 和 capture-only 路径即使没有完整 `CheckReport` 也能追溯
到具体 scenario；这仍不宣称移动语义/输入或真实设备矩阵已通过。
现阶段仍未完成
preview/移动 capture-only 路径的完整 scenario 语义/输入验收、MCP、
复现包或性能验证闭环。

35 项工作包更新为 **1 done、22 in_progress、12 planned、0 in_review**。
O03 保留已有 `done`；S01/M01/M03/T06 从过时的 `planned` 改为 `in_progress`；
F01/T01/P01/T02/T03 因仍缺工作包要求的实现或验收，从 `in_review` 校正为 `in_progress`。
这不是完成百分比，也不表示这些任务的已有实现被撤回。

T05 已合并输入索引、live watcher/session 接入、大型输入本机对照，以及外部 Cargo path package
watch/index。外部来源按稳定 package identity 映射到逻辑 slot，Cargo.toml 改动会刷新 scope 与 watcher
roots；含外部 build.rs 的 package 在该 package root 内稳定全量重扫，未声称发现其目录外实际读集。
已加入 macOS/Linux 文件系统 allow-list、Windows volume/drive 判别和未知类型全量扫描降级。显式
build/observe 同步继续全量稳定核验；性能对照仍只来自 Apple M2/macOS 单环境、一个固定合成输入集。
build-script 声明/实际读集和网络/用户态文件系统跨平台对照仍未闭合，因此 T05 保持 `in_progress`，
不能把单机基准外推为跨平台性能承诺。

| 门槛 | 已有进展 | 未收口部分 |
| --- | --- | --- |
| G0 | headless 基线、target-aware doctor、macOS 观察 PoC | 完整平台基线、版本解析/兼容规则、PoC 未支持的能力 |
| G1 | 窗口/心跳、资源 ACK、产物库、macOS best-effort observe | v1 兼容、真实历史升级、窗口实际环境、same-scene/present 与完整故障验收 |
| G2 | schema、三个 preview、query/diff、动作、check/baseline、租约/构建键、单场景及 matrix frozen inputs、per-cell context/target BuildKey、target-specific output layout/preview output-root lock、matrix cell CheckReport、cleanup-finalized matrix context/capture-only lifecycle evidence、mobile runner identity/capability/run-identity/bounded-event-log evidence、移动截图 manifest/identity verification、移动 semantics 默认 admission 收紧、Android plugin/included-build signing cache bypass hardening、Android wrapper checksum 与依赖 artifact verification、Android 动态依赖 cache gate、Android NDK compiler-tool/sysroot/header 与选定 SDK package、Java runtime content fingerprint、compiler/linker executable content fingerprint、Rust compiler selector environment/content inputs、Cargo build flags/profile override inputs、普通 build/run 与 desktop/iOS simulator/Android default-debug/显式 debug custom-signing/Android release-only signing debug preview 的 BuildKey coordinator、Windows coordinator state publish transient permission-denied bounded retry、reference-aware caller-cancel 与 owned process termination、显式 coordinator `Cancelled`/`Partial` 终态、owner heartbeat/fencing、iOS physical signing BuildKey 边界、受控 Android local custom/release signing build/run、受控 Android signing-sensitive frozen preview build、移动 preview delegated lease/same-run evidence、pre-ready mobile launch failure evidence、mobile cleanup error evidence、action 失败 operation ID evidence、follower 取消和 superseded leader process-tree 终止、cleanup/fixture identity hardening、early mobile scenario failure finalization | partial artifact 的消费/恢复契约、Android 复杂/远端 signing cache hit 与完整输入闭包、真实环境适配、移动 semantics provider 与完整 scenario steps/cleanup、真实连续验收及 MCP |
| G3 | 两种移动 runner、进程证据、matrix admission/并行调度/control/native capture、同一 delegated lease 下的 run/capture/log/cleanup evidence、cleanup-finalized matrix evidence、capture-only cell 的非伪造报告边界、runner identity/capability/run-identity/bounded-event-log evidence、移动截图 manifest/identity verification、pre-ready launch failure evidence、cleanup error evidence、action failure operation ID evidence、Android capture 的 best-effort viewport/scale/orientation/foreground metadata、移动 semantics 默认 admission 不再误报支持 | 完整三端同快照矩阵、真实设备环境元数据与故障/重连、移动 semantics provider/完整语义与输入证据、repro 和 L2/L3 CI |
| G4 | 普通构建缓存复用、签名感知的 iOS physical build/run、受控 Android local custom/release build/run、受控 Android unsigned release build/run、受控 Android signing-sensitive frozen preview build、desktop/iOS simulator/Android default-debug/显式 debug custom-signing/release-only signing debug live preview verified cache hit、Android 固定 Gradle wrapper checksum 与标准 wrapper distribution 内容 fingerprint、distribution/全局配置在 BuildKey 计划前及 Gradle 后重核验、本地 Gradle build logic cache bypass、普通 app Gradle script 保守静态 I/O marker cache bypass（含 `file(...)`/`files(...)` 路径解析、`srcDir(...)`/`srcDirs(...)` source-root 声明、Java NIO/File 派生路径（含 `Path.toFile()`）、Java `File` 状态/枚举/变更 API、NIO `Files`/`FileSystem` 状态/流/变更 API、buffered/data/object stream wrapper、Properties stream API）、未知 Gradle plugin signing behavior 保守 cache bypass、非标准/custom repository cache bypass、Gradle buildscript classpath/add plugin cache gate、Gradle provider/project file I/O marker cache bypass、Gradle custom provider/ValueSource marker cache bypass、Gradle dependency verification SHA-256 metadata、动态/changing dependency cache bypass、NDK compiler-tool/sysroot/header、选定 SDK package、Java runtime 与 compiler/linker executable content fingerprint、Rust compiler selector environment/content inputs、Cargo build flags/profile override inputs、显式清理；T05 已覆盖外部 Cargo path package live watcher/index 并有一个大型输入集本机对照；T06 frozen build plan 按 Cargo package 声明对 workspace 与 external path package 的默认/自定义 build script 保守 bypass artifact cache reuse，`build=false` 不误触发 | 缓存输入遗漏、iOS physical live preview、Android 复杂/远端 signing cache hit 与 release preview cache hit、标准远端 repository runtime/state 和仍未被 marker 识别的 Gradle/plugin/build-script I/O、T05 build-script 实际读集/网络及用户态文件系统跨平台证据、跨平台性能对照、共享构建/预热、性能指标/预算和 Agent 基准 |

G2/G4 补充：PR #295 对已知 Gradle user-home 配置与 Gradle/JVM 注入环境 fail-closed，只禁用复用并继续普通构建；PR #296 让 cache-disabled Android build/preview 不发布新的可复用 manifest；PR #298 扫描 wrapper distribution 的 `init.d` 自定义入口；PR #300 将标准 wrapper layout 下所有已安装 Gradle distribution 内容及相邻状态文件纳入 Android toolchain fingerprint/BuildKey；PR #302 在普通 build、matrix/live preview 的复用前及 Gradle 完成后重核验当前 user home、全局配置和 distribution fingerprint，变化时继续产出 APK 但不消费/发布旧 BuildKey manifest；PR #304 对本地 `buildSrc`/`build-logic` 自定义 Gradle plugin 直接 bypass cache reuse；PR #306 对 Android root 下非 `buildSrc`/`build-logic` 的 `.gradle`/`.gradle.kts` 脚本增加保守静态 marker gate，识别未建模的 provider/env、文件、网络、进程、`apply from` 或 `includeBuild` 入口时只 bypass cache reuse，并保留已建模的 GPUI 参数、NDK `source.properties` 与受控签名读取；PR #308 对无 signing 配置的 unsigned release APK 开放非 live BuildKey artifact cache hit，并对未知 Gradle plugin/alias/plugin-owned signing behavior 保守 bypass；PR #310 对非标准/custom repository 声明直接 bypass cache，标准 repository allowlist 保持模板 cache 路径。非标准布局、缺失安装或有界扫描失败只禁用 cache reuse；完整动态 Gradle/plugin I/O、未被 marker 识别的脚本行为、复杂/远端 signing、release preview 与远端仓库状态仍未闭合，相关工作保持 `in_progress`。

PR #312 对 Android Gradle `buildscript` 中的 `classpath` 做保守插件 gate：只有字面量、固定版本的
`com.android.tools.build:gradle:<version>` 保持 cache eligibility；未知坐标、动态版本、version-catalog
或非字面量 classpath 只关闭 cache reuse，普通构建继续。该切片仍不闭合 plugin 实现的任意运行时 I/O、标准
远端 repository runtime/state、复杂/远端 signing、release preview 或真实设备验收。

PR #314 对 Android Gradle app script 增加 provider/project file I/O marker：`providers.fileContents`、
`projectDirectory`/`projectDir`/`rootDir`、`gradleLocalProperties`、archive-entry provider 和 provider
file materialization 只关闭 cache reuse，普通构建继续。该切片仍不等于完整 Gradle DSL/runtime read-set
追踪，也不闭合标准远端 repository runtime/state、复杂 signing 或 release preview。

PR #316 对 Android Gradle app script 的 `ProviderFactory` custom provider/`ValueSource` 入口增加保守 marker：
`providers.of`、`providers.provider` 和 `ValueSource` 只关闭 cache reuse，普通构建继续。该切片不解析
ValueSource 实现或 provider closure 的实际读集，标准远端 repository runtime/state、复杂 signing 和 release
preview 仍未闭合。

PR #318 对 Android Gradle `buildscript` 中 `add("classpath", ...)` 的动态依赖写法复用同一保守插件 gate：
只有字面量、固定版本的 `com.android.tools.build:gradle:<version>` 保持 cache eligibility；未知坐标和
非字面量/catalog 值只关闭 cache reuse，普通构建继续。该切片不闭合 plugin 实现任意运行时 I/O、标准远端
repository runtime/state、复杂/远端 signing、release preview 或真实设备验收。

PR #320 扩展 Android Gradle app-script I/O marker，单独识别 `file(...)`/`files(...)` 路径解析调用：未建模
路径只关闭 cache reuse，普通构建继续；模板中由 GPUI build-dir 参数驱动的路径和 NDK
`source.properties` 路径保留为已建模例外。此静态 marker 不追踪返回对象的后续使用或任意 Gradle runtime
I/O；标准远端 repository runtime/state、复杂/远端 signing、release preview 和设备验收仍未闭合。

PR #322 将 `srcDir(...)`/`srcDirs(...)` source-root 声明纳入同一静态 gate：未建模的 source root 只关闭
cache reuse、普通构建继续；模板 JNI root 在 `gpui.jniLibsDir` provider 与 `sourceSets` 上下文中保持放行。
这不追踪所有 Gradle source provider 或文件变更，完整运行时读集、标准远端 repository runtime/state、复杂
signing、release preview 和设备验收仍未闭合。

PR #325 补齐 source-root DSL 的替代入口：`setSrcDirs(...)`、`srcDirs = ...`、Groovy command-style
`srcDir 'path'` 和 `srcDirs += ...` 均只关闭 Android artifact cache reuse，普通构建继续；模板受控 JNI
`srcDirs(gpuiJniLibsDir)` 例外保留。注释和字符串中的类似文本不触发 gate。该保守静态扫描仍不追踪
source provider/runtime 文件集合，也不闭合标准远端 repository runtime/state、复杂 signing、release preview
或真实设备验收。

PR #327 补齐 Groovy command-style `file 'path'` / `files 'path'` 路径解析，以及 `srcDirs files 'path'`
形式的 source-root 输入；未建模调用只关闭 Android artifact cache reuse，普通构建继续。注释和字符串中的
类似文本不触发 gate。该保守静态扫描仍不追踪返回对象后续使用、source provider/runtime 文件集合或完整
Gradle 输入闭包，也不闭合标准远端 repository runtime/state、复杂 signing、release preview 或真实设备验收。

PR #329 将 Java/Kotlin `File(...)`、`java.nio.file.Paths.get(...)` 和 `java.nio.file.Path.of(...)`
路径构造也纳入同一静态 marker；限定与非限定类名均只关闭 cache reuse，普通构建继续，注释和字符串不触发。
该 gate 不追踪构造对象后续读取或 Gradle 完整运行时读集。workspace 全量测试并行运行时有 4 个现有
coordinator/devserver 子进程时序测试失败，四项均在单线程单独重跑时通过；这不替代常态全量测试的稳定性问题。
标准远端 repository runtime/state、复杂 signing、release preview 和真实设备验收仍未闭合。

PR #331 扩展 Android Gradle app-script 的静态 file-I/O marker，识别 `ClassLoader` resource lookup、
`ServiceLoader` 和 `Class.forName` 动态类加载入口；这些未建模入口只关闭 cache reuse，普通构建继续，
注释和字符串不触发。该 gate 不追踪类加载后的任意 I/O、plugin/runtime 行为或完整 Gradle 输入闭包。
标准远端 repository runtime/state、复杂 signing、release preview 和真实设备验收仍未闭合。

PR #333 将 `from(...)` 与 Groovy `from 'path'` file collection 配置纳入 Android Gradle app-script
marker，覆盖 `layout.files.from(...)`、自定义 file collection 与 source-set `from`；普通构建继续，只有
cache reuse 被关闭。检测要求方法调用后存在参数，普通 `from` 变量、注释和字符串不触发。它不追踪集合后续
内容变化或完整 Gradle runtime I/O，标准远端 repository runtime/state、复杂 signing、release preview 和设备
验收仍未闭合。

PR #335 将 Java NIO/File 的派生路径入口 `FileSystems.getDefault().getPath(...)`、
`File.getCanonicalFile()`、`File.getCanonicalPath()`、`File.getAbsolutePath()` 和 `File.toPath()` 纳入同一
Android Gradle app-script 静态 marker；命中时只关闭 cache reuse，普通 Gradle 构建继续。回归覆盖限定/非限定
形式以及注释和字符串排除。该 gate 不追踪派生路径对象后续读取、plugin/build-script 任意运行时 I/O 或完整
Gradle 输入闭包；标准远端 repository runtime/state、复杂/远端 signing、release preview 和真实设备验收仍未闭合。

PR #338 将 Java `File` 的权限/元数据查询、目录枚举和常见文件系统变更方法纳入 Android Gradle app-script
marker，包括 `canRead/canWrite/canExecute`、大小/时间/空间查询、`list/listFiles`、创建/删除/重命名与权限
变更；这些新增 API 只在 receiver 方法调用形态下触发，只关闭 cache reuse，普通构建继续。注释、字符串及
模板 clean task 的 `Delete::class`/`delete(rootProject.layout.buildDirectory)` 不触发。首轮本地 Android smoke
发现裸 `delete` marker 把模板 clean task 误判为未建模 I/O；收窄为 receiver-call 后，debug/release miss→hit
smoke 通过。静态 gate 不追踪文件系统状态的运行时闭包，也不等于完整 Gradle 输入闭包或真实设备验收；
标准远端 repository runtime/state、复杂/远端 signing 和 release preview 仍未闭合。

PR #341 扩展到 Java NIO `Files` 状态/属性查询、目录流、读写和文件系统变更 API，并覆盖
`FileSystems.getFileStores()`/`getRootDirectories()`、`FileStore` attributes 与 `Path.toRealPath()`；方法入口
通过 receiver-call marker 只关闭 cache reuse，普通 Gradle 构建继续。回归确认注释和字符串不触发；首轮
Windows CI 中既有 coordinator 并发时序测试断言 state file 尚未出现而失败，单独重跑后通过。该 marker
不构成任意 Gradle/provider/plugin 运行时 I/O 追踪、完整输入闭包或真实设备验收；标准远端 repository
runtime/state、复杂/远端 signing 和 release preview 仍未闭合。

PR #345 将 Java `ZipFile`/`JarFile`、ZIP/JAR 输入输出流及 `ZipFileSystemProvider` 构造入口加入 Android
Gradle app-script 静态 I/O marker，并保留 `FileSystems.newFileSystem(...)` 覆盖。命中只关闭 cache reuse，普通
构建继续；注释、字符串、普通 app source 和模板 clean task 不触发。workspace、Android debug/release 打包及
CLI miss→hit smoke、fmt/clippy/build/design docs/package list/diff check 均通过；PR 与 push 两套
Linux/macOS/Windows、desktop-template、android-template 和 baseline-driver CI 全绿，squash merge
`70d7c5c`。这不追踪归档对象后续 I/O，也不等于完整 Gradle/plugin 输入闭包或真实设备验收；标准远端
repository runtime/state、复杂/远端 signing 与 release preview 仍未闭合。

PR #349 将 Java `Scanner`、`PrintStream`、`PrintWriter` 文件构造器加入同一 code-aware Android Gradle
app-script marker；限定/非限定类名及 File/Path/String 参数形态只关闭 cache reuse，普通构建继续。注释、字符串、
普通 app source 与模板 clean task 不误触发。workspace 456 passed、1 ignored，fmt/clippy/build/design docs/
package list/diff check 与 Android debug/release packaging、CLI miss→hit smoke 通过；PR/push 两套
Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿，squash merge `4d8b251`。
这是保守静态构造器 marker，不追踪构造后对象的读写或完整 Gradle/plugin 输入闭包；标准远端 repository
runtime/state、复杂/远端 signing、release preview、真实设备验收仍未闭合，T06 保持 `in_progress`。

PR #352 将 Java NIO `Path.toFile()` 加入 Android Gradle app-script 静态 file-I/O marker；覆盖限定/非限定
形式，并确认注释、字符串和普通 `app/src/main` source 不触发。命中时只关闭 cache reuse，普通 Gradle 构建继续。
workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release
packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过；PR/push 两套 Linux/macOS/Windows、desktop-template、
android-template、baseline-driver 全绿，squash merge `7473a5b`。这是静态 marker 扩展，不追踪派生对象后续
读写、Gradle/plugin 任意运行时 I/O 或完整输入闭包；标准远端 repository runtime/state、复杂/远端 signing、
release preview 和真实设备验收仍未闭合，T06 保持 `in_progress`。

PR #355 将 Java NIO 文件系统观察入口纳入 Android Gradle app-script 静态 I/O marker，覆盖 `FileSystem`/
`FileSystems`、`WatchService`、`WatchKey`、`WatchEvent`、`Watchable`、事件类型及 watcher factory；新入口命中时
只关闭 cache reuse，普通 Gradle 构建继续。回归覆盖 watcher 创建/注册/事件读取，并确认注释、字符串、普通
app source、普通 `tasks.register` 与模板 clean task 不误触发。workspace 456 passed、1 ignored，fmt/clippy/build/
design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke
通过；PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿，squash merge
`dd316eb`。这是保守静态 gate，不跟踪 watch 注册目标、事件来源或完整 Gradle/plugin/runtime I/O，也不构成真实
设备验收；标准远端 repository runtime/state、复杂/远端 signing 和 release preview 仍未闭合，T06 保持
`in_progress`。

PR #357 将 Java NIO `AsynchronousFileChannel` 与 `SeekableByteChannel` 加入 Android Gradle app-script 静态
I/O marker；限定/非限定 API 引用均只关闭 cache reuse，普通 Gradle 构建继续。回归确认通道 factory/type
入口触发，注释、字符串和普通 app source 排除。workspace 456 passed、1 ignored，fmt/clippy/build/design docs/
package list/diff check、真实 Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过；
PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿，squash merge
`90174a7`。该静态 marker 不验证异步操作完成/回调或 channel 的实际文件读集，也不追踪完整 Gradle/plugin I/O；
真实设备验收、标准远端 repository runtime/state、复杂/远端 signing 与 release preview 仍未闭合，T06 保持
`in_progress`。

PR #360 将 Java NIO `FileSystemProvider`、`DirectoryStream` 与 `SecureDirectoryStream` 加入 Android Gradle
app-script 静态 I/O marker；命中类型/接口引用时只关闭 cache reuse，普通 Gradle 构建继续。回归覆盖 provider
与 stream 使用，并确认注释、字符串和普通 app source 不触发。workspace 456 passed、1 ignored，fmt/clippy/build/
design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke
通过；PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿，squash merge
`550732e`。这是接口级静态 gate，不跟踪 provider 的实际后端、stream 迭代得到的文件或完整 Gradle/plugin I/O；
真实设备验收、标准远端 repository runtime/state、复杂/远端 signing 和 release preview 仍未闭合，T06 保持
`in_progress`。

PR #363 将 Java NIO `Files.walkFileTree`、`FileVisitor`、`SimpleFileVisitor`、`FileVisitResult`、
`FileVisitOption` 和 `BasicFileAttributes` 加入 Android Gradle app-script 静态 I/O marker；walk 调用及 visitor
类型/回调入口只关闭 cache reuse，普通 Gradle 构建继续。回归覆盖 `FOLLOW_LINKS` 遍历、visitor callback，注释、
字符串、普通 app source 和模板 clean/task registration 排除。workspace 456 passed、1 ignored，fmt/clippy/build/
design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke
通过；PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全绿，squash merge
`b7b1949`。静态 marker 不评估遍历范围、visitor 决策产生的实际文件集合或完整 Gradle/plugin I/O；真实设备验收、
标准远端 repository runtime/state、复杂 signing 和 release preview 仍未闭合，T06 保持 `in_progress`。

PR #366 将 Java NIO `PathMatcher` 与 `FileSystem.getPathMatcher(...)` 加入 Android Gradle app-script 静态 I/O
marker；matcher 类型和工厂调用只关闭 cache reuse，普通 Gradle 构建继续。回归覆盖 glob/pattern matcher 的
限定/非限定引用，并确认注释、字符串、普通 app source 不触发。workspace 456 passed、1 ignored，fmt/clippy/
build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit
smoke 通过；PR/push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 最终全绿，
squash merge `e0c36ed`。该 gate 不解析 matcher 实际匹配的路径或完整 Gradle/plugin 输入集合；真实设备验收、
标准远端 repository runtime/state、复杂/远端 signing 和 release preview 仍未闭合，T06 保持 `in_progress`。

G4 的 Android app-script 静态 I/O marker 现也覆盖 Java NIO 异步文件通道、provider/stream 接口与 file-tree
visitor：`AsynchronousFileChannel`、`SeekableByteChannel`、`FileSystemProvider`、`DirectoryStream`、
`SecureDirectoryStream`、`Files.walkFileTree`、`PathMatcher` 和 `FileSystem.getPathMatcher` 引用只使 artifact cache reuse bypass，不阻止
普通 Gradle 构建。检测不判断异步操作/回调完成、provider 后端、遍历范围或实际文件集合；没有据此宣称完整
Gradle 输入闭包。

PR #369 补齐 Gradle 字符串内的表达式执行入口：双引号字符串（含三重双引号）中的 `${...}` 只关闭 Android
artifact cache reuse，普通构建继续；转义后的字面文本、注释与模板简单 `$gpuiAbis` 引用保持原有策略。
它不求值或追踪表达式，即使纯计算也保守 bypass。新增 native-input 回归先复现环境读取被漏过，再确认修复；
workspace 共 490 passed、1 ignored（CLI 单元测试 457 passed），fmt/clippy/build/design docs/package list/
diff check、Android debug/release packaging、双 ABI 和 CLI miss→hit smoke 均通过。真实 NDK/Gradle 对照
保持源码及 BuildKey 相同，只将插值环境值从 `2.1.0` 改为 `2.2.0`，`aapt2` 确认 APK versionName 与内容
hash 随之变化，两次均不发布 reusable manifest，原有 debug/release manifests 未被覆盖。
PR/push 两套三 OS、desktop/android template 和 baseline-driver 首次全绿，squash 为 `41a182e`。
这只补齐一类静态 cache gate；完整 Kotlin/Groovy 语义、Gradle/plugin 输入闭包和真实设备验收仍未闭合，
T06 保持 `in_progress`。对照输入、hash 和 CI 链接见
[T06 Android CLI cache smoke](../experiments/T06-android-cli-cache-smoke-2026-09-28.md)。

PR #372 将受控 NDK/keystore stream 读取例外收窄到每次调用。一个有效的 `source.properties` 或
`keystore.properties` 读取不再放行同脚本里的其他文件、receiver、动态路径、方法引用或 alias import；
未知读取只关闭 cache reuse，普通构建继续。已知字面调用、普通 import、注释和空白保持原策略。
native-input 回归修复前失败、修复后通过；workspace 491 passed、1 ignored（CLI 单元测试 458 passed），
本地格式/clippy/build/docs/package/diff 检查与 Android debug/release packaging、双 ABI、CLI miss→hit 通过。
真实对照保持源码和 BuildKey 不变，只修改项目外的受控测试文件，`aapt2` 确认 APK versionName 从 `3.1.0`
更新为 `3.2.0`，hash 改变，两次均不发布 reusable manifest，原有 manifests 保留。PR/push 两套三 OS、
desktop/android template、baseline-driver 首次全绿，squash 为 `4d4f863`。该检查匹配调用形式，不证明变量
绑定或完整 Gradle/plugin 读集；GUI/设备与 T06 完整验收仍未闭合。

PR #375 将 Java buffered/data/object stream、reader/writer wrapper API 纳入 Android Gradle app-script
静态 I/O marker。限定/非限定 API 引用和构造调用只关闭 artifact cache reuse，普通 Gradle 构建继续；回归覆盖
类型引用、注释和字符串排除。workspace 459 passed、1 ignored，fmt/clippy/build/design docs/package list/diff
check 通过；PR/push 两套 Linux/macOS/Windows、desktop/android template 与 baseline-driver CI 全绿，squash 为
`18c719a`。这是保守静态 gate，不追踪 wrapper 的底层 stream 来源或完整 Gradle/plugin I/O，T06 仍为
`in_progress`；本轮未运行 GUI/设备验收。

PR #377 将 `Properties.load`、`loadFromXML`、`store` 和 `storeToXML` 纳入 Android Gradle app-script
静态 I/O marker。任意 opaque stream、其他 receiver、方法引用或写出操作只关闭 artifact cache reuse；仅保留
模板精确的 NDK `source.properties` 与 keystore `FileInputStream("keystore.properties")` 读取例外。回归覆盖
受控例外、其他 Properties receiver、注释/字符串和普通构造；workspace 459 passed、1 ignored，fmt/clippy/build/
design docs/package list/diff check 通过；PR/push 两套 Linux/macOS/Windows、desktop/android template 与
baseline-driver CI 全绿，squash 为 `852ddec`。这仍是保守静态 gate，不闭合完整 Gradle/plugin I/O 或真实设备验收，
T06 保持 `in_progress`。

本轮还校正实施清单总表中 T05 残留的 `planned`，改为其详细进展已记录的 `in_progress`，并同步下表的
外部 path-package watcher 和文件系统策略摘要；这不增加新的 T05 运行时或跨平台验收证据。

## 2. 相对上次审计的新合并

### T06 Android Gradle cache input closure updates

| 提交 | 新进展 | 判定边界 |
| --- | --- | --- |
| `bb5c10f` | Android runner adapter 接入普通运行 | 不能再写“Android adapter 未实现” |
| `8f1a4ce`、`31e403e`、`ea37ddf` | Android 启动身份、iOS 进程探测、移动 fault evidence 分类 | 不等于完整真实设备故障矩阵或持续日志归属已通过 |
| `942fec4`、`b41aee5`、`87e6af9` | matrix plan/scheduler、执行 adapter、target/scenario/host/ABI/toolchain admission | 不可用 cell 保留原因，required/optional 分开汇总 |
| `54069f1`、`301912d`、`42cdfba` | 并行 executor、资源锁、mobile lifecycle、`gpui check --matrix` | 同一 supervisor 的资源锁不替代主机设备租约；尚未共用冻结快照 |
| `b405cca`（#145） | 移动 matrix 通过 preview/control 执行场景并接 native capture | 单场景 `check --target ios/android` 仍不支持；移动入口是 matrix |
| `14f81fe`（#146） | 单场景和 matrix preview 专属 session key、定向发现；Android preview 元数据/fixture 接入 | 修复旧会话误绑定路径；不同时修复桌面进程清理或 fixture hash |
| `a1395d3`（#147） | `CheckReport.context` 保存 reset generation、environment、uncontrolled inputs | 单场景 JSON 可见；matrix 汇总尚未传递 context，报告值也不等于实际环境已受控 |
| `1256bb3`（#149） | BuildKey 环境 allowlist 纳入 `CARGO_ENCODED_RUSTFLAGS`，并增加 hash 回归测试 | 修复该已确认的缓存键遗漏；不等于所有 build.rs/Gradle/NDK/Xcode 隐藏输入或整个 M04/T06 已完成 |
| `5b63656`（#151） | desktop check preview 使用 process-group/Windows Job Object 归属，cleanup/Drop 终止整个 owned process tree；新增后代管道回归测试 | 修复 supervisor-only cleanup 路径；真实 GPUI check 的完整 GUI 进程验收、fixture hash 和其他 S04 门槛仍未完成 |
| `3e8b874`（#153） | reset 重新计算当前 fixture SHA-256，新的 `scenario_ready` 和单场景 `CheckReport` 使用 runtime fixture identity | 修复 reset 后报告仍保留启动 hash 的路径；真实 GUI reset probe、matrix report 传播和冻结构建仍未完成 |
| `12b1338`（#155） | 单场景 desktop check 通过 `desktop_build_plan` 创建并校验冻结 workspace snapshot，从 snapshot root 启动 preview；`CheckContext` 暴露 snapshot hash 和 BuildKey | matrix check 保持旧路径，尚未共享同一冻结 snapshot；local `build.rs` 等未建模输入会让严格 check 直接不可用，不回退到可变目录 |
| `97406bb`（#157） | matrix 在 admission 前创建并重核验一个 workspace snapshot，从快照重读 scenario/matrix 配置，所有 cell preview 复用同一 runtime root/hash；已知 local `build.rs` 输入继续拒绝严格执行 | target-specific BuildKey/构建输出编排和 MatrixReport context/完整步骤传播仍未接入；真实三端矩阵仍未验收 |
| `85e9c0d`（#159） | matrix cell execution 现在将 `CheckReport.context` 传入 `MatrixReport`，保留 reset generation、environment、uncontrolled inputs 和 shared snapshot hash | MatrixReport 仍未保留完整 steps/cleanup 详情；target-specific BuildKey/构建输出编排和真实三端矩阵仍未验收 |
| `ae9ae94`（#161） | matrix 按 ready target 从共享 frozen root 计算 desktop、iOS simulator、Android ABI-specific BuildKey，并写入每个 cell 的 `CheckContext.build_key` | 只提供 key evidence；target-specific Cargo/Gradle/Xcode 输出编排、共享构建/在途任务合并、完整 steps/cleanup 和真实三端矩阵仍未验收 |
| `4f35c95`（#163） | matrix preview 将 target BuildKey 绑定到 source-project 的 Cargo target、iOS DerivedData、Android JNI/Gradle 输出布局；同一 target BuildKey 的 cell 通过 matrix resource 串行 | 不覆盖跨命令共享构建/在途任务合并、cache hit/coalescing、完整 steps/cleanup 或真实三端矩阵验收 |
| `cca46b3`（#165） | preview desktop/iOS/Android builder 在 source-project 的 target-specific output root 取得 `BuildOutputLock`；root 路径随受控环境传入 preview，并增加跨进程释放回归测试 | 只保护进入该 preview lock 路径的输出变更；不提供 cache hit、manifest 复用、跨命令构建所有权或在途任务 coalescing |
| `5e5031a`（#167） | matrix control scenario cell 将完整 `CheckReport` 保留到 `MatrixCellResult`，JSON 包含 steps、action/assertion/capture evidence、primary error、cleanup 和 context，并增加反序列化 round-trip 回归测试 | 不为 admission-unavailable 或 capture-only mobile lifecycle cell 伪造 scenario report；不覆盖真实三端连续验收或跨命令构建所有权 |
| `7283614`（#169） | desktop preview 将 BuildKey hash 通过受控环境传入，在 output root 的独立 preview manifest 经过 platform/key/file/content hash 验证且只有一个可执行文件时跳过 Cargo；manifest 缺失、损坏或歧义回退构建 | 仅覆盖 desktop preview；不覆盖 iOS/Android preview cache hit、跨命令构建所有权、在途任务 coalescing 或隐藏输入建模 |
| `f6801e9`（#171） | iOS live preview 将 BuildKey hash 通过受控环境传入，在 output root 发布并校验独立 iOS `.app` manifest；simulator 命中时跳过 rustup/Cargo/XcodeGen/`xcodebuild`，physical device 仍强制重建 | 真机签名 identity/Provisioning Profile 等输入仍未建模；不覆盖 Android preview、跨命令构建所有权、在途任务 coalescing 或隐藏输入建模 |
| `0c2771e`（#173） | Android live preview 将 cache policy、ABI 和 default debug keystore hash 传入 preview；在 output root 校验 JNI staging 与 debug APK 输出的完整 manifest，命中时跳过 rustup/cargo-ndk/Gradle | release/custom/sensitive signing、keystore 变化、工具链身份不可读或隐藏输入会 bypass；不覆盖跨命令构建所有权、在途任务 coalescing 或完整设备验收 |
| `14339b5`（#175） | `BuildOutputLock` 在取得 OS 锁后原子写入 `.build-owner.json`，记录 schema、owner、PID、开始时间、状态和可选 BuildKey；释放时仅删除 owner_id 匹配的记录，stale 记录由下一个持锁者覆盖，cache size 统计排除该元数据 | 这是跨进程 ownership 证据，不是 coordinator、心跳/fencing 状态机、subscriber/取消引用或同 key 在途任务合并；OS 锁仍是活跃性唯一权威 |
| `5f6859d`（#177） | 普通 desktop/iOS/Android build/run 的可复用 BuildKey 通过持久 coordinator record 选举单一 leader；同 key follower 持有订阅文件 OS 锁、等待终态并复核完整 artifact manifest，失败可共享、leader 消失可接管，cache clean 避开活跃 subscriber | 尚未接入 live preview/check 构建；无调用者取消引用计数、无订阅者归零后的终止策略、无 heartbeat/fencing/partial 状态机；含未建模输入的路径仍不共享 |
| `e6aeb62`（#189） | subscriber 文件名绑定 attempt identity；active count 以 OS lock probe 判定，不读取 locked JSON；failed attempt sharing 按 attempt 隔离 | active count 仍不是完整 last-reference cancellation/state machine；无 heartbeat/fencing |
| `f4e10c2`（#190） | 记录 subscriber identity/count 状态与验证边界 | 状态文档，不增加运行时行为 |
| `16cf04b`（#191） | preview coordinator 在 leader revision superseded 时终止 owned process tree 并发布 retryable terminal marker；follower 释放旧引用后重新竞争；state lock 串行化注册、计数、record 与 cache-clean subscriber 检查；follower 单独取消不停止 leader | 仍无 `cancelled`/`partial` 状态或完整 last-reference policy；current leader 即使没有 follower 仍可完成自己的构建；无 heartbeat/fencing |
| `4b1a309`（#193） | coordinator 新增 caller-cancel reason；leader 在同一 state lock 内释放自身引用并统计剩余 subscribers，有 follower 时保留 terminal result，无 follower 时发布 retryable cancellation marker 并允许下一 leader；superseded reason 保持原有重建语义 | live preview/check 尚未接入 caller-cancel API；无自动 cooperative process termination、独立 `cancelled`/`partial` state 或 heartbeat/fencing |
| `3364c58`（#195） | desktop、iOS simulator、Android default-debug preview 安装 leader control 到 owned process loop；最后引用 caller-cancel 会终止 owned process tree 并发布 marker，有 follower 时 leader 脱离但共享构建继续；superseded 仍重建旧 attempt | 仍无独立 `cancelled`/`partial` state、heartbeat/fencing、physical/signing-sensitive preview 共享或完整真实设备验收 |
| `f8a95ce`（#197） | `BuildCoordinatorState::Cancelled` 明确表示无剩余 subscriber 的 caller-cancel；follower 放弃旧 attempt 并重新竞争，普通失败仍为 `Failed`，superseded 仍是 retryable `Failed` marker | 尚无 `Partial` 终态/部分产物消费契约、heartbeat/fencing、physical/signing-sensitive preview 共享或完整真实设备验收 |
| `0e05087`（#199） | 增加 `BuildCoordinatorState::Partial`；精确 partial marker 保留不完整输出诊断，但不验证/共享/命中该输出，旧 attempt 的 follower 放弃并重新竞争；error chain 支持 contextual marker | partial marker 需调用方显式返回；没有自动 partial 检测、可消费部分 manifest 或恢复机制，heartbeat/fencing、physical/signing-sensitive preview 共享及完整真实设备验收仍未完成 |
| `13fc5c8`（#201） | coordinator schema v2 增加 owner fencing token/heartbeat；leader 持锁续 heartbeat，terminal publish 校验 owner，stale heartbeat 只作诊断且不能绕过 output OS lock 接管 | 仍是本机 coordinator heartbeat，不提供远端租约/跨主机时钟语义；partial artifact 消费/恢复、签名敏感输入和完整真实设备验收仍未完成 |
| `d20dadc`（#203） | physical iOS BuildKey 纳入 code-signing identity 与 provisioning profile 内容摘要；缺失时禁用复用，构建前后重核验，签名可用时允许 physical manifest 命中 | 未解析 profile entitlement/team/bundle 匹配、私钥可用性或远端自动签名状态；Android release/custom signing、隐藏输入和完整真实设备验收仍未完成 |
| `142f58b`（#205） | Android 非 live build/run 对受控本地 custom/release signing 建模：解析固定 `keystore.properties` 的 `storeFile`，只接受项目内普通 keystore；properties/keystore 内容摘要进入 native BuildKey，敏感文件以短生命周期 `0600` snapshot 副本提供给 Gradle，构建和 manifest 发布前后重核验 | 当时的 preview 路径尚不共享 signing-sensitive 产物；复杂/插件/远端 signing、项目外/绝对路径、软链接、畸形输入保持 cache bypass，隐藏输入和真实设备验收仍未完成；后续 #209/#249 扩展了受控 preview 范围 |
| `a8a484f`（#209） | Android live preview 对显式 `buildTypes.debug.signingConfig` 的 custom-debug signing 纳入 signing fingerprint，verified JNI/APK manifest 与 preview coordinator 可复用；构建前后和 manifest 验证前后重核验输入 | 当时 release-only、复杂 DSL、插件/远端 signing 继续 bypass；#249 后 release-only 本地 signing 仅可支持受控 debug preview，release APK 与复杂/远端 signing 仍 bypass；Gradle/AGP/NDK 隐藏输入和真实设备验收仍未完成 |
| `058d2c6`（#211） | matrix mobile preview 由 supervisor 持有唯一 OS lease，子进程使用不含原始 token 的 delegated owner 校验；capture/native logs/stop evidence 绑定实际 preview run，finalize 后写入 `CheckContext.mobile_evidence`，日志无法归属时保持 inconclusive | 修复 lease/evidence 归属边界，不等于真实 iOS/Android 矩阵、完整语义/输入、键盘/旋转/重连或设备故障验收已通过 |
| `8da659e`（#213） | Android capture 在 PNG artifact 上补充 `wm size`/`wm density` 推导的逻辑 viewport 与 scale、`dumpsys input` 方向和明确前台标记中的目标包名；探测失败保持 unknown，并向 check evidence 暴露缺失原因 | 仅完成 best-effort parser/attachment 与纯 Rust 边界测试；未完成真实设备厂商差异、前台切换、旋转/DPI 变体或完整 M01/M02 验收 |
| `2087d52`（#215） | scenario executor 在初始 prepare/reset/observation 失败时先调用 finalization 再 cleanup，保留已启动移动 preview 的 native-log/post-run evidence，同时保留原始失败 | 只修复证据收集顺序；未 ready 的 preview 仍不得通过，finalization 失败和真实设备连续验收仍按 inconclusive/未完成处理 |
| `9379d9d`（#217） | action operation 错误详情含 `operation_id` 时，scenario `StepReport.action` 保留 action status 与 operation ID，包括 unknown/cancelled/unavailable/timeout 路径 | 只增强 operation 归属证据，不改变 step/check 主结果；unknown 仍为 inconclusive，不能重放或伪造成 passed |
| `7b6f4ae`（#219） | 移动 preview 在 `scenario_ready` 前 launch/registration 等待失败时，先尝试 native logs、owned stop 和 lease release，并把 evidence/artifact ids 返回失败 matrix cell；未绑定实际 run 时标记 `run_id_bound=false`，不生成伪造 `CheckReport` | 只补 pre-ready 失败的证据保留；没有 scenario steps 时不声称完整场景执行，真实设备连续验收仍未完成 |
| `1ef1f97`（#221） | mobile cleanup 独立记录 stop/release 错误到 `mobile_evidence.cleanup_errors`，stop 失败时仍释放 lease，并保留 logs/run 归属证据 | 只增强 cleanup 诊断与资源释放边界，不等于真实设备连续验收已完成 |
| `5e30dae`（#223） | matrix executor 在串行/并行 cleanup 后优先读取 runner context；mobile matrix adapter 将 launch、capture、native logs、stop、lease release 和 cleanup errors 写入 `MatrixReport.context.mobile_evidence`，capture-only cell 保留 evidence 但不生成伪造 scenario report | 只补 cleanup-finalized report wiring 和 capture-only 证据可见性，不等于完整 scenario 语义/输入或真实三端矩阵验收 |
| `cbc0bc1`（#225） | capture-only matrix 与 control-driven scenario 两条移动路径将 `RunnerInfo`/`RunnerCapabilities` 写入 `mobile_evidence`，绑定 runner、host、平台、架构、设备类型、工具声明和能力 | 这是声明性 runner metadata，不等于设备状态、前台身份、旋转/DPI 实探针或真实矩阵验收 |
| `d7f383a`（#227） | 移动 runner 的 `EvidenceLog` 通过统一 trait 暴露，并在 capture-only matrix 与 control-driven scenario evidence 中保留 install/launch/capture/process/channel/log/stop 事件序列 | 只接通已有事件日志的报告传播，不等于真实设备 fault matrix、重连或连续验收 |
| `1f98372`（#229） | `EvidenceLog` 增加 128 条事件与 16 KiB detail 上限，淘汰最旧事件并保留最新 stop/log 证据；超限时保留摘要/hash，暴露 `truncated`/`dropped_events`，反序列化也执行边界并保持序号继续递增 | 只完成事件日志的内存/JSON 有界性，不增加 crash、ANR、重连、前台 probe 或真实设备 fault matrix 证据 |
| `dda6ffe`（#231） | capture-only mobile matrix context 补齐 `CaptureArtifact` 的 provider/path/bytes/hash、逻辑 viewport、scale、方向、系统 UI 和前台包名字段，避免报告只剩像素尺寸与 run id | 只补证据传播，不等于厂商 display probe 稳定、真实前台切换/旋转/DPI 验收或完整设备矩阵已通过 |
| `0073cf4`（#233） | capture-only mobile matrix context 保留 prepare 阶段的 `run_identity`，包括 run/project/device、lease session 和 fencing token 摘要；原始 fencing token 不进入 JSON，身份缺失时以 `run_id_bound=false` 表示 | 只补同一 run/lease 的可核对证据，不等于真实设备 lease 竞争、重连、token 轮换或完整移动 scenario 验收已通过 |
| `0e11688`（#235） | control-driven mobile scenario evidence 补齐 `project_id`，并在 `run_id_bound=true` 时发布嵌套 `run_identity`；scenario_ready 前保持 null，原始 fencing token 仍不进入 JSON | 只统一 scenario 路径的身份证据形状，不等于 preview run 绑定之外的真实设备连续验收、重连或 token 轮换已通过 |
| `119f6ec`（#237） | iOS/Android capture event detail 记录已取得的 artifact/hash、像素/逻辑尺寸、scale、方向、系统 UI、前台 marker 和 run id；不记录 host path 或 lease secret | 只增强有界事件序列的诊断可见性，不增加缺失的设备探针、真实前台切换或连续设备验收 |
| `6bd62f0`（#239） | 每条 `EvidenceEvent` 记录 `project_id`、`lease_session_id` 和 `fencing_token_sha256`，从同一 `RunIdentity` 写入事件级 run/project/device/lease/fencing 摘要；原始 fencing token 不进入 JSON，并补充反序列化与敏感信息回归测试 | 只让单条事件自带可核对的身份摘要，不增加真实 lease 竞争、重连、token 轮换或设备矩阵验收 |
| `67d1160`（#241） | 移动 capture-only matrix 与 control-driven check 在发布证据前重新验证 PNG 文件类型、大小、字节数、SHA-256、尺寸和 artifact ID；被替换/损坏的输出不会进入截图证据 | 当时只完成发布前完整性核验；完整截图 manifest、真实设备 fault matrix、重连或连续验收由后续切片推进 |
| `b781339`（#243） | 每张移动截图原子发布 schema 1 sidecar manifest，绑定 artifact/hash/bytes/PNG 尺寸、逻辑 viewport/scale/方向、系统 UI、前台 marker 与 run/project/device/lease/fencing digest；check/matrix 发布前复核 manifest 与当前 `RunIdentity` | 完成截图 manifest 与同一 run identity 的结构化归属；原始 fencing token/绝对宿主路径不进入 manifest/report，但截图 evidence 的 manifest 路径暴露由后续切片补齐，真实设备 fault matrix、重连和连续验收仍未完成 |
| `47e32ba`（#245） | `ScreenshotEvidence` 暴露相对于 artifact root 的可选 `manifest_path`，screenshot assertion evidence 同步携带该字段；移动 check 规范化分隔符并拒绝绝对路径、artifact root 外路径，补充路径泄漏边界测试 | 只增强已验证 sidecar 的报告可见性和路径安全边界；不提供远程 artifact store、绝对宿主路径或真实设备 fault matrix/重连/连续验收 |
| `68ea03b`（#247） | mobile lifecycle evidence 绑定 cell/target/scenario、requirements 和 frozen fixture hash；pre-ready、cleanup failure、capture-only evidence 在无完整 `CheckReport` 时仍保留 scenario contract | 只补 scenario 归属证据，不增加移动语义/输入能力、真实设备状态或连续矩阵验收 |
| `90585ef`（#249） | Android debug live preview 对受控本地 release-only `signingConfig` 纳入 release keystore fingerprint 和默认 debug keystore hash；verified JNI/APK manifest 与 preview coordinator 仍只缓存 debug preview | 仅在静态证明 release 变体使用受控 keystore、debug 变体未覆盖默认签名且所需 keystore 可验证时复用；release APK、缺少默认 debug keystore、复杂 DSL、插件/远端 signing 仍 bypass；不补齐 Gradle/AGP/NDK 隐藏输入或真实设备验收 |
| `23a4f08`（#251） | BuildKey coordinator 原子写入状态文件时，Windows `PermissionDenied`/`Access is denied` 可触发最多 5 次指数退避重试（5–80ms 间隔）；保留原子替换、短状态锁和 fencing 语义 | 只缓解短暂文件访问冲突；超过有界重试或其他错误仍失败。首次 push CI 的同 key 并发用例曾复现失败，定向重跑通过；PR 与 push 两套 CI 最终全绿，不代表所有 Windows 文件系统/杀软竞争已穷尽 |
| `cbd55b8`（#253） | 本地 matrix admission 不再为 iOS/Android 默认静态声明 `semantics`、`semantics.read`、`semantics.bounds`；无 capability override 时语义依赖 cell 在派发前 unavailable，显式 capability override 路径有回归覆盖 | 只修正 admission 的静态默认声明；override 是调用方声明而非自动证据验证；不实现 GPUI mobile semantics provider、不改变 runtime hello，也不证明 accessibility 树在真实设备可用；移动语义 scenario 与真实设备验收仍未完成 |
| `a5ab033`（#255） | Android signing marker 检查覆盖 Gradle root 内 Kotlin/Groovy/Java 插件源码；`settings.gradle(.kts)` 使用 `includeBuild` 时，即便外部插件实现不可见也禁用 cache reuse，并增加两项回归测试 | 只扩大保守 cache bypass，不改变正常 frozen build；没有实现复杂/远端 signing cache hit，也没有闭合 Gradle wrapper、AGP/plugin、NDK/build-script 的所有隐藏输入 |
| `2a5782d`（#257） | Android 模板为 Gradle 9.4.1 配置并经官方 endpoint 核验 distribution SHA-256；Android artifact cache reuse 要求 frozen `gradle-wrapper.properties` 中恰有一个有效 64 位 checksum，checksum 本身随 wrapper properties 进入 BuildKey；matrix preview 的 cache policy 改从相同 frozen snapshot 读取 | checksum 缺失、重复或格式错误只禁用缓存，不阻止正常构建；只验证 Gradle wrapper 下载包完整性，不覆盖 AGP/plugin/NDK/build-script 的其他隐藏输入，也不表示 Android 全链路环境已冻结 |
| `35b0afc`（#259） | Android cache reuse 检查 Gradle 脚本、版本目录及 `buildSrc`/`build-logic` Kotlin/Groovy/Java 插件源码；动态版本、版本范围、SNAPSHOT、latest 与 changing-module 配置会绕过 artifact cache；固定版本仍可命中 | 只禁用不稳定依赖输入下的 cache reuse，不阻止正常构建；源码检查是保守静态扫描，不闭合 AGP/plugin 仓库制品、NDK 或任意 build-script I/O 输入 |
| `51552aa`（#261） | Android toolchain fingerprint 纳入当前 host NDK `clang/clang++`、`lld/ld.lld`、LLVM archiver/inspection tools 及 ABI clang launcher 内容和符号链接目标；相同 revision 下编译工具替换会改变 BuildKey | 只覆盖 host compiler/linker 与 ABI launcher，不哈希完整 NDK/sysroot、AGP/plugin artifacts 或任意 build-script I/O；Windows MSVC 使用 sha2 纯 Rust 路径，其他平台使用汇编加速 |
| `8777b8d`（#263） | Android toolchain fingerprint 进一步纳入当前 host NDK sysroot 与 Clang builtin headers 的有界内容树摘要；相同 NDK revision 下替换 header/library 会改变 BuildKey；symlink/special entry、读取失败或超过 100,000 项/512 MiB 时 cache bypass | 普通 Android build 仍可运行；只闭合 sysroot 与 builtin headers，不包含 AGP/plugin 制品或任意 build-script I/O；host fingerprint 约 9.6 秒 |
| `2a713da`（#265） | Android toolchain fingerprint 按项目字面量 `compileSdk` 选择对应 platform，并按显式 `buildToolsVersion` 或 SDK 中最新数值版本选择 build-tools；将选定 package 的相对路径、entry type 和内容摘要纳入同一 100,000 项/512 MiB 有界预算 | 同 revision 下替换选定 SDK package 文件会改变 BuildKey；动态/无法解析的 SDK 选择、缺包、软链接、special entry、读取失败或超预算只关闭 cache reuse；不扫描未使用的已安装包，不闭合 AGP/plugin resolved artifacts 或任意 build-script I/O |
| `bdfd717`（#267） | Android 模板新增 Gradle dependency verification metadata，为 Android plugin/传递依赖、debug/release 使用的制品及 macOS/Linux/Windows AAPT2 classifier 固定 SHA-256；Android build plan 和 preview 只在 metadata 严格且每个 artifact 都有 hash 时允许 cache reuse | metadata 自身随 native input 进入 BuildKey，缺失/畸形/非严格/存在 trusted-artifacts 放行项时 cache bypass，Gradle 构建仍执行；不闭合远端仓库状态或任意 build-script I/O |
| `4aa6ed3`（#269） | Android toolchain identity 读取实际 Java `java.home`/`java.version`，将活动 JDK 树内容纳入有界 fingerprint；JDK 内部目录链接递归纳入，外部目录链接以逻辑路径纳入，外部文件链接只纳入文件内容 hash，不写绝对路径 | JDK 内容不可读、断链、循环、特殊文件或超过 100,000 entries/512 MiB 时只关闭 cache reuse；未改变 NDK/SDK 的严格 symlink 规则，不闭合远端仓库或任意 build-script I/O |
| `17b369d`（#271） | 路径/hash/mtime/大小/file identity 输入索引，支持 dirty path、rename/delete 双向失效、有界事件队列、overflow 与读取竞态全量回退 | 首个实现切片；watcher 接入和大项目对照由后续切片完成 |
| `a995989`（#272） | live watcher 回调只标 dirty 并排队，session 主循环增量刷新；build/observe/sync 继续完整稳定核验；目录变动、incomplete rename、watcher overflow 回退 | 外部 Cargo path dependency/build-script 自声明读集和自动网络盘判别尚未纳入 |
| `c2ddaa3`（#273） | 增加忽略的手动大型输入 benchmark 和扫描 pass/hash 字节统计；Apple M2 release、4096×8 KiB、10 warmup + 30 paired runs，wrong_revision_acceptance=0 | 单机、热文件系统缓存、单文件变更的合成基准；不等于跨 OS/网络盘性能结论或完整 T-09 输入范围 |
| `de97a17`（#275） | Cargo path-package roots 纳入 live session 输入索引与 watcher，按稳定 package identity 分配逻辑 slot；Cargo.toml 改动刷新 scope 并增删 watcher roots；外部 build.rs 变化触发该 package root 稳定全量重扫；filesystem metadata policy 在 macOS/Linux/Windows 上对未知/网络类型降级；manifest、冻结 relocation 与诊断不包含外部源绝对路径 | 不解析 build.rs 声明/实际目录外读集；文件系统类型 allow-list 与 fallback 已实现，但网络/用户态文件系统和跨平台性能对照仍需持续证据 |
| `657fd93`（#277） | frozen desktop/iOS/Android build plan 的 cache policy 识别 workspace 与 external path package 的 `build.rs` 和 Cargo `[package].build` 自定义脚本，明确禁用 artifact cache reuse；保留冻结副本和正常构建路径，补充外部 custom build-script 端到端回归且诊断不泄露绝对路径 | 这是保守 bypass，不是 build-script 实际文件/环境/网络读集发现；Gradle/NDK/Xcode hidden I/O、远端状态和 cache hit 仍未闭合 |
| `2629f82`（#279） | cache policy 按 Cargo package manifest 解释 build-script 声明：无显式 `build` 时仅 package 内存在默认 `build.rs` 才 bypass，`build=false` 不触发，字符串自定义脚本仍 bypass；workspace/external path package 共用该规则并有回归 | 仍是保守 bypass；不发现脚本实际文件/环境/网络读集，也不闭合 Gradle/NDK/Xcode hidden I/O、远端状态或 cache hit |
| `c5003b0`（#281） | 增加冻结 desktop build plan 回归：Cargo package 设置 `build=false`、源码树仍有 `build.rs` 时，脚本文件会进入冻结快照但不关闭 cache eligibility；默认/自定义脚本和 external package 的 bypass 语义保持 | 仍只验证 Cargo manifest 声明边界；不发现 build.rs 实际文件/环境/网络读集，也不把正常冻结/构建宣称为完整输入闭包 |
| `d132fe9`（#283） | BuildKey 环境摘要纳入 `RUSTC_WORKSPACE_WRAPPER`，以及所选 target 对应的 Cargo linker/Rust flags；分别增加变化和 target 隔离回归 | 修复已确认的编译环境键遗漏；不等于任意 wrapper 内容、build.rs/Gradle/NDK/Xcode 隐藏 I/O 或远端状态已闭合 |
| `f911394`（#285） | 对 `RUSTC_WRAPPER`/`RUSTC_WORKSPACE_WRAPPER` 指向的可执行文件做有界 SHA-256/大小指纹；内容变化改变 BuildKey，无法解析、读取竞态、非普通文件或超过 64 MiB 时仅禁用 cache reuse；matrix desktop/iOS preview 同步采用该 gate | 不递归 wrapper 的依赖/子进程/环境隐式读取；不闭合 build.rs/Gradle/NDK/Xcode 任意 I/O 或远端状态 |
| `a385d8a`（#287） | BuildKey 环境摘要补齐 `AR`、`CFLAGS`、`CXXFLAGS`，各变量变化均有 hash 回归 | 仅覆盖显式 native 编译环境变量；不代表工具链所有隐式输入或任意构建脚本读取已闭合 |
| `caf03eb`（#289） | 将 Rust wrapper、`CC`、`CXX`、`AR` 与每个选定 target 的 `CARGO_TARGET_<TARGET>_LINKER` 统一扩展为有界可执行文件内容指纹；相同路径替换内容会改变 BuildKey；matrix desktop/iOS/Android cache gate 分别绑定实际 target triple 或 ABI 对应的 Rust target | 命令参数或 shell 语法、缺失/非普通文件/不可执行文件、读取竞态、非 Unicode 环境值或超过 64 MiB 预算时只禁用 cache reuse，普通构建继续；不闭合工具启动的子进程、额外环境/文件/网络读取以及 build.rs/Gradle/NDK/Xcode 任意隐藏 I/O |
| `5c0cbde`（#291） | 将 `RUSTUP_TOOLCHAIN`、`RUSTC_BOOTSTRAP` 纳入 BuildKey 环境摘要，并对显式 `RUSTC` 选择的可执行文件复用有界内容指纹；相同路径替换内容会改变摘要，缺失/参数化/不可安全读取时普通构建继续但禁用 cache reuse | 只闭合显式 Rust compiler selector 环境；未闭合默认 `rustc` 之外的 Cargo 可执行文件/global Cargo 配置、编译器子进程或 build.rs/Gradle/NDK/Xcode 隐藏 I/O |
| `b2dc719`（#293） | 将 `CARGO_BUILD_RUSTFLAGS` 及 dev/release profile 的 debug、assertions、codegen-units、incremental、LTO、opt-level、overflow-checks、panic、rpath、split-debuginfo、strip 覆盖纳入 BuildKey 环境摘要；每项变化都有 hash 回归 | 只覆盖 CLI 当前 dev/release profile 的显式 Cargo 环境覆盖；custom profile、Cargo 可执行文件/global Cargo 配置、build.rs/Gradle/NDK/Xcode 隐藏 I/O 与远端状态仍未闭合 |

| 提交 | 新进展 | 判定边界 |
| --- | --- | --- |
| `dbb275d`（#295） | Android build plan 与 preview cache gate 检查 Gradle user-home 的 `gradle.properties`、`init.gradle(.kts)`/`init.d`，以及 Gradle/JVM 注入环境；配置存在或 home 不可解析时只禁用 artifact cache reuse，普通构建继续，诊断不含 home 路径或配置值 | 这是已知全局配置面的保守 bypass，不读取/指纹化配置内容；不扫描未显式指定 `GRADLE_HOME` 时 wrapper 解压 distribution 内部的 init 脚本，也不闭合任意 build-script I/O 或 plan 后并发变化 |
| `8e44e44`（#296） | Android 普通 build 与 live preview 在 cache reuse 不安全时不再发布新的可复用 artifact manifest；普通 build 直接返回已验证存在的 APK，避免未建模 Gradle 配置下产物污染同一 BuildKey 的后续 cache hit | 不改变安全路径的 verified-manifest 命中；不把全局配置冻结或加入 BuildKey，也不提供 Android 设备验收 |
| `a10dd0c`（#298） | Gradle global-config gate 有界检查 wrapper user-home 下各个已解压 distribution 的 `init.d`；只放行官方 `readme.txt`，其他 entry、symlink、不可读目录或超过 4096 个扫描项都禁用 Android cache reuse | 不读取脚本内容或输出路径；只覆盖 installation init scripts，不指纹化整个 Gradle distribution，也不消除 plan 后的并发变化 |
| `b0767ab`（#300） | Android toolchain fingerprint 对标准 `GRADLE_USER_HOME/wrapper/dists` 下所有已安装 Gradle distribution 做有界相对路径/内容摘要，并纳入 BuildKey；相邻非 `.lck` wrapper 状态文件也进入摘要。自定义安装布局、缺失 distribution、symlink、读取失败或超过 100,000 entries/512 MiB 预算时只禁用 cache reuse | 不冻结或在 build 前后重核验 user home；不闭合 plan 后并发变化、任意 Gradle/plugin/build-script I/O 或远端仓库状态 |
| `1c8cbd5`（#302） | 普通 Android build 与 matrix/live preview 携带 Gradle distribution identity；复用前和 Gradle 后重核验当前绝对 user home、全局配置/环境及内容 fingerprint。变化时普通构建/预览保留 APK，但不发布或消费旧 BuildKey manifest；相对 `GRADLE_USER_HOME` 的 preview 保守 bypass，policy BuildKey 与 Android build key 使用同一输入模型 | 不闭合 Gradle 执行期间除 distribution/已知 global gate 外的任意 plugin/build-script I/O、远端仓库状态或真实设备验收 |
| `ccdecf5`（#304） | 检测 `mobile/android/gradle/buildSrc` 与 `build-logic` 下的本地 Gradle build logic 输入；存在时只禁用 Android cache reuse，普通 build/preview 仍继续 | 不读取或指纹化 plugin 任意 I/O；不覆盖普通 app Gradle script、远端 plugin/repository 状态或真实设备验收 |
| `16083d0`（#306） | 对 Android root 下非 `buildSrc`/`build-logic` 的 `.gradle`/`.gradle.kts` 脚本执行保守静态扫描；未建模的 provider/env、文件读取、网络/进程、`apply from` 或 `includeBuild` marker 只关闭 cache reuse，普通 build/preview 继续；注释、字符串和普通 app source 不触发，GPUI ABI/输出目录/NDK `source.properties`/受控签名读取保留为已建模例外 | 不是 Gradle 解析器或运行时 I/O 追踪；未识别的 plugin/script I/O、远端 repository metadata/state、复杂 signing 与真实设备验收仍未闭合 |
| `9d9cad5`（#308） | 无 signing 配置的 unsigned Android release APK 纳入非 live BuildKey cache reuse；未知 Gradle plugin/alias/plugin-owned signing behavior 保守关闭 cache reuse；release preview、复杂/远端 signing、敏感输入和普通 app 未建模 I/O 仍不复用 | 只覆盖静态可证明的 unsigned release artifact；不提供 release preview、custom/remote signing cache，不把未知 plugin 的静态 marker 当作完整运行时 I/O 追踪 |
| `bbcf21e`（#310） | 对 Android Gradle `repositories {}` 做保守静态 gate；仅允许 `google()`、`mavenCentral()`、`gradlePluginPortal()`，自定义 `maven`、`mavenLocal`、`flatDir`、`exclusiveContent` 和其他 repository entry 只关闭 cache reuse，普通 build/preview 继续 | 不读取或 fingerprint 标准远端 repository runtime/state；标准仓库状态、未识别 repository/plugin I/O、复杂 signing 与真实设备验收仍未闭合 |
| `d5abe82`（#312） | 对 Android Gradle `buildscript` 的 `classpath` 做保守插件 gate；固定版本的已知 AGP 坐标保持 cache eligibility，未知坐标、动态版本、version-catalog 或非字面量输入只关闭 cache reuse，普通构建继续 | 只覆盖静态可证明的 AGP classpath；不闭合 plugin 实现任意运行时 I/O、标准远端 repository runtime/state、复杂 signing 或 release preview |
| `9584673`（#314） | 对 Android Gradle app script 增加 provider/project file I/O marker；`providers.fileContents`、project/root directory access、`gradleLocalProperties`、archive-entry provider 和 provider file materialization 只关闭 cache reuse，普通构建继续 | 只覆盖已知静态 marker；不提供 Gradle DSL/runtime read-set 追踪，不闭合标准远端 repository runtime/state、复杂 signing 或 release preview |
| `a248be9`（#316） | 对 Android Gradle app script 的 `ProviderFactory` custom provider/`ValueSource` 入口增加 marker；`providers.of`、`providers.provider` 和 `ValueSource` 只关闭 cache reuse，普通构建继续 | 不解析 ValueSource/provider closure 的实际读集；不闭合标准远端 repository runtime/state、复杂 signing 或 release preview |
| `c754013`（#318） | 对 Android Gradle `buildscript` 的 `add("classpath", ...)` 增加保守插件 gate；固定版本的已知 AGP 字面量坐标保持 cache eligibility，未知坐标和非字面量/catalog 值只关闭 cache reuse，普通构建继续 | 只覆盖静态可证明的 AGP classpath add；不闭合 plugin 实现任意运行时 I/O、标准远端 repository runtime/state、复杂 signing 或 release preview |
| `822030c`（#320） | Android app-script I/O marker 识别未建模的 `file(...)`/`files(...)` 路径解析调用，只关闭 cache reuse、普通构建继续；GPUI build-dir 与 NDK `source.properties` 的已建模读取仍可复用 | 不解析返回对象后续使用或 Gradle runtime I/O；不闭合标准远端 repository runtime/state、复杂 signing 或 release preview |
| `dfc023c`（#322） | Android app-script I/O marker 识别未建模 `srcDir(...)`/`srcDirs(...)` source-root 声明，只关闭 cache reuse、普通构建继续；由已建模 `gpui.jniLibsDir` 驱动的模板 JNI source root 保持 cache eligible | 仅识别静态 source-root 调用及模板例外；不解析 source provider/runtime 变化，标准远端 repository runtime/state、复杂 signing 与 release preview 仍未闭合 |
| `1475d24`（#323） | 文档基准更新到 PR #322 squash merge，记录 source-root cache gate、模板 JNI 例外、验证证据和未闭合边界 | 仅文档一致性更新；不增加 runtime 或平台验收 |
| `1b765ee`（#325） | 扩展 source-root marker 至 `setSrcDirs(...)`、`srcDirs` 属性赋值和 Groovy command-style `srcDir`/`srcDirs +=` 声明；仅关闭 cache reuse，保留模板 JNI 例外 | 保守静态 DSL marker，不追踪 provider/runtime 内容或完整 Gradle 输入闭包 |
| `9c0fef8`（#327） | 扩展 file marker 至 Groovy command-style `file`/`files` 和 `srcDirs files` 路径声明；仅关闭 cache reuse，注释/字符串不触发，普通构建继续 | 保守静态路径 marker，不追踪返回对象后续使用、source provider/runtime 内容或完整 Gradle 输入闭包 |
| `cc1da49`（#329） | 扩展 file marker 至 Java/Kotlin `File(...)`、`Paths.get(...)`、`Path.of(...)`（含限定与非限定形式）；仅关闭 cache reuse，普通构建继续，注释/字符串不触发 | 不追踪构造对象后续读取或 Gradle 完整运行时读集；远端仓库状态、复杂 signing 与设备验收仍未闭合 |
| `8a4d4b4`（#331） | 扩展 Android Gradle app-script I/O marker 至 `ClassLoader` resource lookup、`ServiceLoader` 与 `Class.forName`；只关闭 cache reuse，普通构建继续，注释/字符串不触发 | 不追踪加载类后的任意 I/O、plugin/runtime 行为或完整 Gradle 输入闭包；远端仓库状态、复杂 signing 与设备验收仍未闭合 |
| `be895ce`（#333） | 扩展 marker 至 `from(...)` 与 Groovy `from 'path'` file collection 输入，覆盖布局文件集合及 source-set；只关闭 cache reuse，普通构建继续 | 不追踪集合后续内容变化或完整 Gradle runtime I/O；远端仓库状态、复杂 signing 与设备验收仍未闭合 |
| `e8d70cd`（#335） | 扩展 marker 至 `FileSystems.getDefault().getPath(...)`、`getCanonicalFile()`、`getCanonicalPath()`、`getAbsolutePath()` 和 `toPath()` 派生路径入口；只关闭 cache reuse，普通构建继续，限定/非限定形式均覆盖，注释和字符串不触发 | 不追踪派生路径对象后续读取或完整 Gradle runtime I/O；远端 repository runtime/state、复杂 signing、release preview 与设备验收仍未闭合 |
| `0e04377`（#338） | 扩展 marker 至 Java `File` 权限/元数据查询、目录枚举和创建/删除/重命名/权限变更 API；receiver 方法调用只关闭 cache reuse，普通构建继续，注释/字符串与模板 Gradle `Delete::class` clean task 不触发 | 不追踪文件系统状态或对象后续 I/O 的运行时读集；完整 Gradle 输入闭包、远端 repository runtime/state、复杂 signing、release preview 与设备验收仍未闭合 |
| `892fc21`（#341） | 扩展 marker 至 NIO `Files` 状态/attribute query、directory stream、read/write/mutation、`FileSystems` store/root enumeration、`FileStore` attribute 和 `Path.toRealPath()`；仅关闭 cache reuse，普通构建继续 | 静态方法名称扫描不闭合任意运行时读集或完整 Gradle 输入；远端 repository runtime/state、复杂 signing、release preview 与设备验收仍未闭合 |
| `7473a5b`（#352） | 将 Java NIO `Path.toFile()` 加入 Android Gradle app-script 静态 file-I/O marker；限定/非限定形式命中，注释/字符串和普通 app source 排除；仅关闭 cache reuse，普通构建继续 | 不追踪派生 `File` 对象后续读写、Gradle/plugin 任意运行时 I/O 或完整输入闭包；远端 repository runtime/state、复杂 signing、release preview 与设备验收仍未闭合 |
| `dd316eb`（#355） | 将 Java NIO `FileSystem`/`WatchService`/`WatchKey`/`WatchEvent`/`Watchable` 与 watcher factory/event-kind 入口加入 Android Gradle app-script 静态 I/O marker；仅关闭 cache reuse，普通构建继续 | 不追踪 watch 目标和事件的实际文件来源、Gradle/plugin 任意运行时 I/O 或完整输入闭包；远端 repository runtime/state、复杂 signing、release preview 与设备验收仍未闭合 |
| `90174a7`（#357） | 将 `AsynchronousFileChannel`/`SeekableByteChannel` 类型与异步文件通道 factory 入口加入 Android Gradle app-script 静态 I/O marker；限定/非限定引用均只关闭 cache reuse | 不验证异步操作完成、回调或 channel 实际文件读集；不闭合 Gradle/plugin runtime I/O、远端仓库状态、复杂 signing、release preview 或设备验收 |
| `550732e`（#360） | 将 Java NIO `FileSystemProvider`、`DirectoryStream`、`SecureDirectoryStream` 类型/接口入口加入 Android Gradle app-script 静态 I/O marker；仅关闭 cache reuse | 不跟踪 provider 后端、stream 实际枚举的文件或完整 Gradle/plugin runtime I/O；远端仓库状态、复杂 signing、release preview 和真实设备验收仍未闭合 |
| `b7b1949`（#363） | 将 `Files.walkFileTree` 与 Java NIO visitor/result/option/attributes 类型加入 Android Gradle app-script 静态 I/O marker；walk 与 visitor callback 入口仅关闭 cache reuse | 不评估遍历范围、visitor 实际访问文件集合或完整 Gradle/plugin runtime I/O；远端仓库状态、复杂 signing、release preview 和真实设备验收仍未闭合 |
| `e0c36ed`（#366） | 将 `PathMatcher` 与 `FileSystem.getPathMatcher(...)` 加入 Android Gradle app-script 静态 I/O marker；matcher/type/factory 引用仅关闭 cache reuse | 不解析实际匹配路径或完整 Gradle/plugin 输入集合；远端仓库状态、复杂 signing、release preview 和设备验收仍未闭合 |
| `41a182e`（#369） | 双引号字符串（含三重双引号）中的 `${...}` 表达式保守 bypass Android artifact reuse；真实同源码/同 BuildKey 环境切换对照确认 APK 版本与 hash 更新，且不发布可复用 manifest | 不求值表达式或追踪完整 Gradle I/O；简单 `$name` 保持现有策略，远端仓库状态、复杂 signing、release preview 和设备验收仍未闭合 |
| `4d4f863`（#372） | 逐次检查受控 NDK/keystore stream 调用，其他文件、receiver 或非字面参数不再借用整份脚本的例外；真实外部文件变化会更新 APK，且不发布 reusable manifest | 静态调用形式不证明变量绑定或完整 Gradle/plugin 读集；真实设备、复杂 signing、release preview 与其余 T06 验收仍未闭合 |
| `18c719a`（#375） | 将 buffered/data/object stream、reader/writer wrapper API 引用加入 Android app-script 静态 I/O gate；限定/非限定引用及构造调用只关闭 cache reuse，普通构建继续 | 不追踪底层 stream 来源或完整 Gradle/plugin I/O；T06 其他输入闭包、远端仓库状态、复杂 signing、release preview 与设备验收仍未闭合 |
| `852ddec`（#377） | 将 `Properties.load`/`loadFromXML`/`store`/`storeToXML` 纳入 Android app-script 静态 I/O gate；仅保留模板精确的 NDK/keystore stream 读取例外，其余 opaque stream、receiver、方法引用和写出只关闭 cache reuse | 不验证 Properties receiver/stream 的完整运行时读集或 Gradle/plugin 任意 I/O；T06 其他输入闭包、远端仓库状态、复杂 signing、release preview 与设备验收仍未闭合 |

代码入口：[check](../../src/commands/check.rs)、[matrix admission](../../src/runner/matrix_admission.rs)、
[matrix executor](../../src/runner/matrix_executor.rs)、[mobile lifecycle adapter](../../src/runner/mobile_matrix.rs)、
[iOS runner](../../src/runner/ios.rs)、[Android runner](../../src/runner/android.rs)、
[mobile capture manifest](../../src/runner/mobile.rs)、
[scenario executor](../../src/scenario/executor.rs)。CLI 的移动场景路径复用 control runner；
独立 mobile lifecycle adapter 的契约测试不能替代该 CLI 调用链的设备验收。

## 3. 全部工作包

| ID | 状态 | 已实现 / 剩余边界 |
| --- | --- | --- |
| F01 | in_progress | 三种严格 fixture、单调 span、有界日志、macOS headless live 驱动器已实跑每 fixture 10 次预热 + 30 次测量，保留 LoginForm 编译失败/恢复样本；PR #380 squash 合并后，本轮在隔离 generated Counter 项目关闭 P-01 macOS cancel、superseded 和 native-install-failure 责任变体，保存 `cancelled`/`superseded` spans、窗口截图、受控 `ios.install` exit 73、未安装 app、lease owner 释放和临时 simulator 删除证据；模板新增带许可证的 iOS-safe `backtrace 0.3.76` 快照，iOS simulator Rust check/Xcode build 已通过。剩余：新模板变更的 CI/review/merge、P-02 T05/T06 联合对照、跨平台 GUI/device 证据 |
| T01 | in_progress | 合并基线有 target-aware JSON doctor、required/optional、有限输出和超时；`doctor_cli` integration test 已通过真实 CLI 生成 desktop-only 项目，覆盖显式 target、项目默认 target 和非项目 host-only，并核对 schema v2、required pass/可选 warning 与移动工具链隔离。T-03 又以 22 台可用 iOS simulator 中的两个 selector 核对名称+runtime 与 UDID 解析，并对 ARM64 AVD 验证匹配与 x86_64 配置不匹配。PR #380 补上 Ubuntu `cc (Ubuntu …) 13.3.0` 版本行解析，并以 squash merge 合并为 `f8192d6`；PR #381/382/383 三平台/template/baseline-driver CI 全绿，PR #383 squash 为 `757f05b`。剩余：真实 Linux/Windows doctor 工具版本矩阵、真实 x86/未知 ABI 设备及 physical-device 选择、未建模 AGP/Gradle 组合边界验收 |
| P01 | in_progress | 有真实 macOS PoC；cancel/superseded/native-install-failure 变体已有隔离窗口、span、iOS simulator build/install-failure、cleanup/lease/device 证据，semantics 明确 runtime_unavailable；scene readback、完整帧/GPU、完整设备证据和父任务 CI/review 仍有限 |
| T02 | in_progress | 模板 manifest、嵌入内容摘要、增平台保护；真实历史内容的稳定取得仍不贯通 |
| F02 | in_progress | 独立 protocol crate、v2、请求关联和 feature 边界；无在线 v1 兼容/事件投影，runtime 仍在模板内 |
| T03 | in_progress | 保守 B/L/N 整文件计划和冲突保护；缺结构化合并及完整历史基线识别 |
| T04 | in_progress | 锁、journal、备份、多文件替换/恢复和并发保护；跨真实发行版升级及升级后的编译验证未齐 |
| O01 | in_progress | 窗口身份、UI heartbeat、迟到/关闭处理；实际 resize/DPI/前台状态和完整故障矩阵未齐 |
| O02 | in_progress | 资源事务、删除、hash、分层 ACK、重连对账；不证明 GPU 呈现，真实完整变体未齐 |
| O03 | done | 产物分块/hash、原子发布、配额、pin/过期和导出；仅覆盖产物库 |
| O04 | in_progress | observe、操作生命周期、build request、macOS window capture；best_effort，不承诺 same_scene/present |
| O05 | in_progress | debug-a11y 快照和显式 logical_id bridge；受激活/provider 限制，非通用完整语义树 |
| O06 | in_progress | observation-bound query/diff、分页和预算；真实大树/虚拟化与单节点 artifact 验收未齐 |
| S01 | in_progress | 静态 schema、fixture 路径/hash、registry 校验已实现；自定义 schema 与完整环境确定性未齐 |
| S02 | in_progress | Counter/LoginForm/VirtualList preview/reset；Android preview 已接入，真实环境适配仍有缺口 |
| S03 | in_progress | click/type/key/scroll、正常事件路径、owner/scope 和 unknown 语义；真实输入/遮挡/污染及持久幂等验收未齐 |
| M03 | in_progress | 主机 OS 锁、owner/fencing、heartbeat，run/live/capture 和移动 matrix 已接入；重连和真实竞争矩阵未齐 |
| M04 | in_progress | 稳定扫描、外部 Cargo path root、普通三端构建冻结/输出隔离/manifest；单场景及 matrix 已消费共享冻结 snapshot/target BuildKey、绑定输出布局并锁定 preview 输出根，普通 desktop/iOS/Android build/run 已接入 BuildKey coordinator，desktop/iOS simulator/Android default-debug/显式 debug custom-signing/release-only signing debug live preview 已接入 verified manifest 命中、caller-cancel、显式 Cancelled/Partial 状态和 owner heartbeat/fencing，Windows coordinator state publish 的瞬态 access-denied 有界重试已接入；Android wrapper distribution checksum 与 Gradle dependency verification metadata 纳入模板并成为 artifact cache reuse 前置条件；physical iOS signing BuildKey、受控 Android local custom/release signing BuildKey 与 signing-sensitive frozen preview build 已接入，BuildKey output ownership record 与移动 preview delegated lease 已接入；partial 输出消费/恢复、远端仓库状态、任意 build-script I/O 及其他 preview 路径仍未齐 |
| S04 | in_progress | executor、desktop check、单场景及 matrix frozen inputs、baseline/diff/approve、移动 matrix 场景 driver、per-cell context/BuildKey/output layout/preview output lock/完整 CheckReport、移动 delegated lease 与 same-run capture/log/stop evidence、owned process-tree/fixture identity cleanup、preview coordinator caller-cancel、显式 Cancelled/Partial 状态和 owner heartbeat/fencing；移动 capture-only 语义/输入、环境及三夹具各 20 次真实验收未齐 |
| A01 | planned | CLI/control 可复用；无 MCP 只读适配、JSON-RPC server 或独立 Agent service |
| A02 | planned | action/operation/check 可复用；无 MCP 动作/取消/owner 适配 |
| A03 | planned | pin/manifest/registry 可复用；无 context 命令、版本知识索引或工作流包 |
| S05 | planned | 无类型化热参数、overlay revision、撤销/固化及收益实验 |
| M01 | in_progress | iOS/Android adapter、capture/log snapshot、进程身份与 fault evidence；移动 preview 已通过 delegated lease 复核 owner，并将 capture/log/stop 绑定实际 run；Android capture 已补 best-effort viewport/scale/orientation/foreground metadata；early scenario failure 也先 finalize evidence；缺完整日志/设备故障、真实环境变体和 UI 变体验收 |
| M02 | in_progress | 配置展开、admission、并行/资源锁、matrix CLI、移动 control/native capture、共享 frozen snapshot/target output lock、control scenario cell 完整报告、移动 delegated lease 与 `CheckContext.mobile_evidence`；本地 iOS/Android admission 不再静态宣称 semantics/read/bounds，但 runtime semantics provider、完整语义/输入 scenario 和三端矩阵仍缺；普通 build/run 已有跨命令构建所有权 |
| M05 | planned | 无 repro export/inspect/run、脱敏和干净环境重放 |
| Q01 | planned | 已有三 OS CLI/macOS 模板/Android 宿主 CI；尚无该工作包的完整真实 GUI/设备 L2/L3 门禁 |
| G01 | planned | supervisor 计时已有；无应用 layout/paint/frame/CPU/GPU 指标与开销验收 |
| G02 | planned | 无 perf 执行器、统计/可比性和性能预算判定 |
| T05 | in_progress | 路径/hash/mtime/大小/file identity 索引已接入 live watcher；Cargo metadata 驱动外部 path-package roots 刷新，含 build script 的 root 保守全量扫描；文件系统 allow-list 限制 metadata 复用，未知/网络/用户态类型回退全量扫描；build/observe 全量稳定核验。4096×8 KiB 合成集已有本机 release 对照，wrong_revision_acceptance=0。剩余：build-script 实际读集及环境/网络输入闭包、实挂载可靠性和跨平台性能对照 |
| T06 | in_progress | desktop/iOS simulator/Android default-debug/显式 debug custom-signing/release-only signing debug preview 缓存与 cache clean，普通 build/run 及受支持的 live preview 已接入同 key coordinator、verified manifest、caller-cancel、显式 Cancelled/Partial 状态和 owner heartbeat/fencing；physical iOS build/run 与受控 Android local custom/release build/run 已按签名输入决定 manifest 复用；无 signing 配置的 unsigned Android release non-live build/run 也已命中 verified artifact cache；Android wrapper checksum、标准 wrapper distribution 内容 fingerprint 与 Gradle dependency verification metadata 纳入 BuildKey/cache policy，缺失/无效/非标准布局/有界扫描失败时 cache bypass，复用前及 Gradle 后的 distribution/global gate 重核验已接入普通 build 与 matrix/live preview，变化时保留普通 APK 但不消费/发布旧 manifest；本地 buildSrc/build-logic 直接 bypass cache reuse；普通 app Gradle script 已增加保守静态 I/O marker bypass，覆盖路径构造、file collection/source-root、类/资源 lookup、Java `File` 状态/元数据/目录枚举/变更 API、NIO `Files`/`FileSystem` 状态/属性/流/读写/变更 API、`Path.toFile()`、WatchService/文件系统观察入口、`Files.walkFileTree` visitor API 和 PathMatcher/getPathMatcher 入口，未知 plugin/alias/plugin-owned signing behavior 与 custom repository 也保守 bypass，已建模 GPUI/NDK/签名读取保留 cache eligibility；相对 `GRADLE_USER_HOME` preview 保守 bypass，Android preview policy 与 build key 共享输入采集；动态/changing Gradle dependency 也只绕过 cache reuse、不阻止构建；NDK host compiler/linker/tool、sysroot 与 Clang builtin headers、选定 SDK package content 纳入 Android toolchain fingerprint；release preview、signed/复杂/远端 signing 仍 bypass，BuildKey output ownership record 已落地；Partial 仅为不可复用的诊断终态；未识别的 Gradle/plugin/repository I/O、标准远端 repository runtime/state、完整任意 app build-script I/O 仍未闭合，预热未实现 |
| Q02 | planned | 仅有 12 项任务设计；无可执行评分器和固定预算对照实验 |
| G03 | planned | 无 GPU capture/analysis provider 闭环；可选 |
| M06 | planned | 无远程 runner、传输和断线恢复；可选 |
| A04 | planned | JSON/diff 已有；无静态审阅器或 MCP Apps；可选 |

## 4. 旧审计问题的当前状态

以下编号延续原报告的七项问题。本轮使用当前二进制/模板和隔离夹具重新核查，
问题 1–4 的旧触发已有局部修复，其余三项仍可复现；不将局部修复扩大为整个工作包完成。

| 编号 | 结论 | 代码与影响 |
| --- | --- | --- |
| 1 缓存环境键 | 已修复该已确认遗漏 | #149 在 [build_inputs.rs](../../src/runner/build_inputs.rs) 的 allowlist 加入 `CARGO_ENCODED_RUSTFLAGS`，并用不同 encoded flag 值证明环境 hash 改变；其他未建模输入仍不因此解决 |
| 2 check 误绑定旧 preview | 已修复该路径 | #146 为单场景和 matrix 注入 session key；[control](../../src/devserver/control.rs) 按 target suffix 定向发现；复测旧 preview 未收到 check reset 且仍存活 |
| 3 桌面 cleanup 假成功 | 已修复 supervisor-only 路径 | #151 让 [check cleanup/Drop](../../src/commands/check.rs) 通过 `OwnedChild` 使用 Unix process group/Windows Job Object，并以 descendant-held pipe 回归测试证明后代随终止关闭；真实 GPUI check 的完整 GUI 进程探针仍未重跑 |
| 4 fixture 变化但 hash 不变 | 已修复 reset/report 路径 | #153 让 [preview reset](../../templates/app/src/previews.rs) 按当前 fixture bytes 重算 SHA-256，新的 ready 事件和 `CheckReport.fixture_hash` 使用 runtime identity；真实 GUI probe 与 matrix 汇总传播仍未完成 |
| 5 v1 在线兼容 | 未解决 | [app channel](../../src/devserver/app_channel.rs) 拒绝 proto 1，[control](../../src/devserver/control.rs) 拒绝 schema 1 |
| 6 真实历史模板升级 | 未解决 | [模板基线解析](../../src/template.rs) 仍缺不可变历史内容；旧 T02 项目会遇到 `baseline_unavailable` |
| 7 doctor 畸形版本 | 未解决 | [probe](../../src/toolchain/probe.rs) 仍按命令退出状态判定，缺版本解析 |

另外两条旧缺口继续有效，并补记本轮确认的 matrix 报告边界：

- 单场景 desktop `check` 已通过 `desktop_build_plan` 创建并重核验 FrozenInputs snapshot；
  matrix check 也在 admission 前创建一个严格 snapshot，所有 cell 从同一 runtime root 启动
  preview，并把原项目 root 仅用于 baseline/diff 和移动 artifact 输出。已知 local `build.rs`
  等未建模输入会让严格 check/matrix 直接不可用，不回退到可变目录；matrix target-specific
  BuildKey 已从 shared snapshot 计算并绑定到 source-project output layout；普通 build/run 的跨命令
  coordinator 已接入，但 check/matrix preview 仍未消费该 coordinator，cache coalescing 和在途任务
  所有权在 preview/check 路径仍未接入。
- [桌面模板](../../templates/desktop/src/main.rs) 仍固定 Light 主题及窗口注册尺寸；
  `ready_environment` 中 theme/locale/clock/seed 来自配置。报告保留这些值改善了追踪性，
  不能代替真实 viewport/DPI、环境控制、字体/backend 和语义 provider 的验证。
- matrix CLI 现在把内部 `CheckReport` 传到 control scenario 的每个 `MatrixCellResult`，因此
  matrix JSON 可见 reset generation、environment、uncontrolled inputs、shared snapshot hash、
  完整 steps/证据和 cleanup；executor 在 cleanup 后重新读取 runner context，使 mobile
  `stop`/lease release/cleanup errors 等后置证据也能进入报告。admission-unavailable cell 与
  capture-only mobile lifecycle cell 保持没有 scenario report，但会保留可取得的 lifecycle
  evidence，不用空报告冒充执行证据。该切片关闭了摘要投影和 cleanup-finalized evidence 缺口，
  但仍不能扩大为真实三端 matrix execution evidence 或实际环境已受控。

## 5. 验证记录与证据范围

以下“上一轮审计”记录针对 `5f6859d`，作为历史证据保留；本地原始输出、隔离探针源码及 JSON 保存在
`artifacts/progress-audit-2026-09-29/`（忽略的本机产物目录，不是已发布验收证据）。
测试独立设置 `GPUI_DEVICE_LEASE_DIR`，避免与其他 worktree 的租约测试互相影响。

| 检查 | 本轮结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --workspace --locked` | 348 个单元测试及全部集成/协议测试通过，0 failed |
| `cargo build --locked` | 通过 |
| `cargo x check-design-docs`、`git diff --check` | 通过；仅验证文档/示例一致性 |
| 两份 35 项状态表逐项比对 | 相同；1 done、22 in_progress、12 planned |
| PR #151 CI | required checks 全部通过；首次 Windows 并发测试波动重跑后通过，三 OS check、desktop-template、android-template、baseline-driver 均通过 |
| PR #155 CI | required checks 全部通过；三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #155](https://github.com/kingsword09/gpui-cli/pull/155) |
| PR #157 CI | required checks 全部通过；三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #157](https://github.com/kingsword09/gpui-cli/pull/157) |
| PR #159 CI | required checks 全部通过；macOS capture helper 初次超时后重跑通过，三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #159](https://github.com/kingsword09/gpui-cli/pull/159) |
| PR #161 CI | required checks 全部通过；三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #161](https://github.com/kingsword09/gpui-cli/pull/161) |
| PR #163 CI | required checks 全部通过；Windows pointer-dispatch 既有测试首次超时后重跑通过，三 OS check、两组 desktop-template、两组 android-template、baseline-driver 均通过；无 release/tag | [PR #163](https://github.com/kingsword09/gpui-cli/pull/163) |
| PR #165 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #165](https://github.com/kingsword09/gpui-cli/pull/165) |
| PR #167 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #167](https://github.com/kingsword09/gpui-cli/pull/167) |
| PR #169 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #169](https://github.com/kingsword09/gpui-cli/pull/169) |
| PR #171 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #171](https://github.com/kingsword09/gpui-cli/pull/171) |
| PR #173 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；macOS capture helper deadline 初次波动后重跑通过；无 release/tag | [PR #173](https://github.com/kingsword09/gpui-cli/pull/173) |
| PR #175 CI | required checks 全部通过；三 OS check、desktop-template、android-template、baseline-driver 均通过；无 release/tag | [PR #175](https://github.com/kingsword09/gpui-cli/pull/175) |
| PR #177 CI | required checks 全部通过；Android template 先修复 follower cache-hit 诊断契约后重跑，Windows live-feedback 时序波动重跑通过；三 OS check、desktop-template、android-template、baseline-driver 最终均通过；无 release/tag | [PR #177](https://github.com/kingsword09/gpui-cli/pull/177) |

本次接续到 `058d2c6` 的增量证据：

| 检查/合并 | 结果 |
| --- | --- |
| 本地运行时验证（PR #197） | 359 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #195/#196 | preview owned-process cancellation 接线及对应状态记录均已 squash 合并；无发布/tag |
| PR #197 CI | PR 与 push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 和文档门槛均通过；push 的 Windows 时序 job 在重跑失败 job 后通过 |
| PR #197 合并 | squash merge `f8a95ce`；无发布/tag |
| 本地运行时验证（PR #199） | 362 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #199 CI | PR 与 push 两套 CI 最终全通过；PR 首轮 Windows 既有 coordinator 时序测试失败后仅重跑失败 job，Windows 和 macOS 均通过，Linux、两类模板和 baseline-driver 通过 |
| PR #199 合并 | squash merge `0e05087`；无发布/tag |
| 本地运行时验证（PR #201） | 365 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #201 CI | PR 与 push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 和文档门槛均通过；无发布/tag |
| PR #201 合并 | squash merge `13fc5c8`；无发布/tag |
| 本地运行时验证（PR #203） | 366 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #203 CI | PR 与 push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 和文档门槛均通过；无发布/tag |
| PR #203 合并 | squash merge `d20dadc`；无发布/tag |
| 本地运行时验证（PR #209） | 375 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #205/#206/#207/#208 | required CI 全部通过；功能与文档切片均以 squash 合并；无发布/tag |
| PR #209 CI | 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #209 合并 | squash merge `a8a484f`；无发布/tag |
| 本地运行时验证（PR #211） | 377 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #211 CI | 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #211 合并 | squash merge `058d2c6`；无发布/tag |
| 本地运行时验证（PR #213） | 378 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #213 CI | 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #213 合并 | squash merge `8da659e`；无发布/tag |
| 本地运行时验证（PR #215） | 379 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #215 CI | 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #215 合并 | squash merge `2087d52`；无发布/tag |
| 本地运行时验证（PR #217） | 380 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #217 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #217 合并 | squash merge `9379d9d`；无发布/tag |
| 本地运行时验证（PR #219） | 381 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #219 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #219 合并 | squash merge `7b6f4ae`；无发布/tag |
| 本地运行时验证（PR #221） | 382 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #221 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #221 合并 | squash merge `1ef1f97`；无发布/tag |
| 本地运行时验证（PR #223） | 383 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #223 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #223 合并 | squash merge `5e30dae`；无发布/tag |
| 本地运行时验证（PR #225） | 383 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #225 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #225 合并 | squash merge `cbc0bc1`；无发布/tag |
| 本地运行时验证（PR #227） | 383 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check 均通过 |
| PR #227 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；失败/取消的既有 macOS/Windows 任务重跑后通过；无发布/tag |
| PR #227 合并 | squash merge `d7f383a`；无发布/tag |
| 本地运行时验证（PR #229） | 387 个单元测试及全部集成/协议测试目标通过；首轮唯一既有 build-coordinator 时序失败单独重跑通过；workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #229 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #229 合并 | squash merge `1f98372`；无发布/tag |
| 本地运行时验证（PR #231） | 387 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #231 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #231 合并 | squash merge `dda6ffe`；无发布/tag |
| 本地运行时验证（PR #233） | 387 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #233 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；首轮 Windows 时序断言失败及 fail-fast 取消的 macOS job 定向重跑后通过；无发布/tag |
| PR #233 合并 | squash merge `0073cf4`；无发布/tag |
| 本地运行时验证（PR #235） | 387 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #235 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #235 合并 | squash merge `0e11688`；无发布/tag |
| 本地运行时验证（PR #237） | 388 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #237 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #237 合并 | squash merge `119f6ec`；无发布/tag |
| 本地运行时验证（PR #239） | 388 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #239 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #239 合并 | squash merge `61beb9c`；无发布/tag |
| 本地运行时验证（PR #241） | 389 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #241 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；Windows 首轮既有时序测试失败后定向重跑通过；无发布/tag |
| PR #241 合并 | squash merge `67d1160`；无发布/tag |
| 本地运行时验证（PR #243） | 390 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #243 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #243 合并 | squash merge `b781339`；无发布/tag |
| 本地运行时验证（PR #245） | 391 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #245 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #245 合并 | squash merge `47e32ba`；无发布/tag |
| 本地运行时验证（PR #247） | 391 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs、diff check、package list 均通过 |
| PR #247 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #247 合并 | squash merge `68ea03b`；无发布/tag |
| 本地运行时验证（PR #259） | 动态 Gradle dependency 定向回归、workspace clippy、fmt、design docs 和 diff check 通过；此前全量 workspace 测试 400 项中唯一既有 coordinator 并发测试瞬态失败，单独重跑通过；无发布/tag |
| PR #259 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；Android smoke 验证固定依赖仍命中 BuildKey cache；无发布/tag |
| PR #259 合并 | squash merge `35b0afc`；无发布/tag |
| 本地运行时验证（PR #261） | 活跃 macOS NDK 的 compiler-tool fingerprint 回归约 5 秒完成（启用 sha2 汇编优化并对 canonical binary 去重）；401 个 workspace 单测及全部集成/协议测试、clippy、fmt、build、package list、design docs 和 diff check 通过；Windows 使用纯 Rust SHA-256 路径 |
| PR #261 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；首轮 Windows 暴露 `sha2-asm` 不支持 MSVC `.S`，将汇编 feature 限定到非 Windows 后重跑全绿；无发布/tag |
| PR #261 合并 | squash merge `51552aa`；无发布/tag |
| 本地运行时验证（PR #263） | sysroot/header 修改使 fingerprint 改变、规范化目录摘要与软链接拒绝回归通过；活跃 macOS NDK toolchain fingerprint 约 9.6 秒；403 个 workspace 单测及全部集成/协议测试、clippy、fmt、build、package list、design docs 和 diff check 通过 |
| PR #263 CI | PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #263 合并 | squash merge `8777b8d`；无发布/tag |
| 本地运行时验证（PR #265） | 选定 SDK platform/build-tools 的内容替换、跨安装路径稳定性、未使用包忽略、显式/默认 build-tools 选择、动态选择拒绝和软链接/预算边界回归通过；406 个 workspace 单测及全部集成/协议测试、clippy、fmt、design docs、package list 和 diff check 通过；强制 Android toolchain probe 本地通过 |
| PR #265 CI | 首轮 Android probe 因错误扫描所有已安装 SDK package 暴露失败；收窄到项目选定 package 后，PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过，Android 生成/打包验证通过；无发布/tag |
| PR #265 合并 | squash merge `2a713da`；无发布/tag |
| 本地运行时验证（PR #267） | dependency verification metadata 缺失、畸形、宽松、无 SHA-256 与 trusted-artifacts 场景回归通过；407 个 workspace 单测及全部集成/协议测试、clippy、fmt、design docs、package list、diff check 通过；Gradle `--write-verification-metadata sha256 assembleDebug assembleRelease` 的结果与模板元数据逐字节一致；Android debug/release APK、两 ABI 与 CLI 第二次 cache hit 通过 |
| PR #267 CI | 首轮真实模板构建发现 AGP 按 host 解析 AAPT2 classifier；补齐并独立校验 Google Maven 的 Linux/macOS/Windows JAR SHA-256 后，PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #267 合并 | squash merge `bdfd717`；无发布/tag |
| 本地运行时验证（PR #269） | Java runtime 版本/home 解析、JDK 内容替换、跨安装路径稳定性、内部/外部文件与目录链接、越界规则和活动 Android toolchain probe 回归通过；411 个 workspace 单测及全部集成/协议测试、clippy、fmt、design docs、package list 和 diff check 通过；Android debug/release APK、两 ABI 与 CLI 第二次 cache hit 通过 |
| PR #269 CI | 前两轮 Android probe 暴露 Temurin JDK 内部/外部 truststore 与目录链接布局；最终按逻辑路径递归纳入外部目录、仅 hash 外部文件且不泄露绝对路径后，PR 与 push 两套 required CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #269 合并 | squash merge `4aa6ed3`；无发布/tag |
| PR #271–#272 合并 | 输入索引类型、watcher/session 增量接入分别 squash 为 `17b369d`、`a995989`；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；无发布/tag |
| PR #273 合并 | 大型输入扫描指标/手动基准 squash 为 `c2ddaa3`；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 419 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #275 合并 | 外部 Cargo path package watcher/index、build-script package root rescan、稳定 slot/路径脱敏及三平台 filesystem fallback squash 为 `de97a17`；CI 首轮发现 Windows verbatim disk path 被当作 UNC，修复后 PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 429 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #277 合并 | frozen build plan 的 build-script cache bypass 扩展到 external path package 与 `[package].build` 自定义脚本；workspace 与 custom external build-script 回归通过；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 430 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #279 合并 | cache bypass 改按 Cargo `[package].build` 语义判定，覆盖默认脚本、自定义脚本和 `build=false`；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 431 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #281 合并 | 增加 `build=false` 且残留 `build.rs` 的冻结 desktop plan cache-eligibility 回归；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 最终全绿；workspace 432 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #283 合并 | BuildKey 纳入 workspace wrapper 与所选 target 的 linker/Rust flags，并通过各维度变化及 target 隔离回归；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 433 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #285 合并 | Rust compiler wrapper 可执行文件指纹与不可安全指纹时的 cache bypass 接入普通三端计划及 matrix desktop/iOS/Android preview gate；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 435 passed、1 个手动 benchmark ignored；无发布/tag |
| PR #287 合并 | BuildKey allowlist 纳入 `AR`、`CFLAGS`、`CXXFLAGS`，每项均有变更 hash 回归；PR 与 push 两套 Linux/macOS/Windows、Android/desktop template、baseline-driver 全绿；workspace 436 passed、1 个手动 benchmark ignored；无发布/tag |
| 本地运行时验证（PR #289） | compiler/linker 工具内容替换、target linker、多 target linker、不可解析命令/参数/shell 语法、不可执行文件和超预算回归通过；workspace 439 passed、1 个手动 benchmark ignored，全部集成/协议测试、workspace clippy、build、Windows target check、fmt、design docs 和 diff check 通过 |
| PR #289 CI | 首轮 Windows clippy 暴露 `std::os::windows::fs::MetadataExt` 的 unstable API，改用稳定 `GetFileInformationByHandle` 后重跑；最终 PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #289 合并 | squash merge `caf03eb`；无发布/tag |
| 本地运行时验证（PR #291） | Rust compiler selector 内容替换、`RUSTUP_TOOLCHAIN`/`RUSTC_BOOTSTRAP` 变化、缺失 `RUSTC` 只禁用 cache reuse 的回归通过；workspace 441 passed、1 个手动 benchmark ignored，全部集成/协议测试、workspace clippy、build、Windows target check、fmt、design docs 和 diff check 通过 |
| PR #291 CI | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #291 合并 | squash merge `5c0cbde`；无发布/tag |
| 本地运行时验证（PR #293） | `CARGO_BUILD_RUSTFLAGS` 与 dev/release profile 覆盖的逐项 hash 回归通过；workspace 442 passed、1 个手动 benchmark ignored，全部集成/协议测试、workspace clippy、build、Windows target check、fmt、design docs 和 diff check 通过 |
| PR #293 CI | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；无发布/tag |
| PR #293 合并 | squash merge `b2dc719`；无发布/tag |
| 本地运行时验证（PR #295） | Gradle user-home 配置/环境 gate、home 解析与无路径/秘密诊断回归通过；workspace 445 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug/release APK 与 CLI miss→hit smoke 通过 |
| PR #295 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `dbb275d`；无发布/tag |
| 本地运行时验证（PR #296） | bypass Android manifest 不发布/不覆盖且直接返回 APK 的回归通过；workspace 445 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 与本机 Android debug/release、CLI miss→hit smoke 通过 |
| PR #296 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `8e44e44`；无发布/tag |
| 本地运行时验证（PR #298） | wrapper distribution `init.d` README allowlist 与自定义脚本 bypass 回归通过；workspace 446 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/package list 通过；本机 Android debug/release APK 与 CLI miss→hit smoke 通过 |
| PR #298 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `a10dd0c`；无发布/tag |
| 本地运行时验证（PR #300） | distribution contents/marker 变化导致 fingerprint 改变、非标准/escaped wrapper layout bypass、超预算与 symlink fail-closed 回归通过；workspace 448 passed、1 个手动 benchmark ignored，focused Gradle/toolchain tests、clippy/build/Windows target check/fmt/design docs/package list 通过；Pinned Gradle wrapper 探测、本机 Android debug/release APK 与 CLI miss→hit smoke 通过 |
| PR #300 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `b0767ab`；无版本发布/tag |
| 本地运行时验证（PR #302） | distribution identity 的复用前/后重核验、当前 user-home/config 变化、相对 user-home preview bypass、policy/build-key 对齐、变化期间保留 APK 且不发布 manifest 的回归通过；workspace 449 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug/release APK 与 CLI miss→hit smoke 通过 |
| PR #302 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `1c8cbd5`；无版本发布/tag |
| 本地运行时验证（PR #304） | 本地 `buildSrc`/`build-logic` 检测与普通 app source 排除回归通过；workspace 451 passed、1 个手动 benchmark ignored，focused build-logic test、clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug/release APK 与 CLI miss→hit smoke 通过 |
| PR #304 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过（push macOS failed job rerun 后通过）；squash merge `ccdecf5`；无版本发布/tag |
| 本地运行时验证（PR #306） | 未建模 Android app Gradle script I/O marker 的 bypass、已建模 GPUI/NDK/签名读取、注释/字符串与 app source 排除回归通过；workspace 451 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug/release APK、ABI 检查与 CLI miss→hit smoke 通过 |
| PR #306 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `16083d0`；无版本发布/tag |
| 本地运行时验证（PR #308） | unsigned release policy、release preview 保持 bypass、未知 plugin/alias/plugin-owned signing marker 回归通过；workspace 454 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug 与 unsigned release APK/ABI 检查、CLI 两种 variant miss→hit smoke 通过 |
| PR #308 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `9d9cad5`；无版本发布/tag |
| 本地运行时验证（PR #310） | 标准 Gradle repository allowlist 与自定义 maven/mavenLocal/flatDir/exclusiveContent bypass 回归通过；workspace 455 passed、1 个手动 benchmark ignored，clippy/build/Windows target check/fmt/design docs/package list 通过；本机 Android debug 与 unsigned release APK/ABI 检查、CLI miss→hit smoke 通过 |
| PR #310 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `bbcf21e`；无版本发布/tag |
| 本地运行时验证（PR #312） | 已知模板 AGP buildscript classpath 保持 cache eligible，未知坐标、动态版本、version-catalog 和非字面量 classpath bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含两个 ABI |
| PR #312 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `d5abe82`；无版本发布/tag |
| 本地运行时验证（PR #314） | provider/project file I/O marker（fileContents、project/root directory、gradleLocalProperties、archive entry、provider file materialization）bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含两个 ABI |
| PR #314 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `9584673`；无版本发布/tag |
| 本地运行时验证（PR #316） | custom provider/ValueSource marker（`providers.of`、`providers.provider`、`ValueSource`）bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含两个 ABI |
| PR #316 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `a248be9`；无版本发布/tag |
| 本地运行时验证（PR #318） | `add("classpath", ...)` 的固定 AGP 放行、未知坐标和非字面量/catalog bypass 回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/Windows target check/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 ABI 检查、CLI miss→hit smoke 通过，缓存 APK 含 `arm64-v8a`/`x86_64` |
| PR #318 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `c754013`；无版本发布/tag |
| 本地运行时验证（PR #320） | 未建模 `file(...)`/`files(...)` bypass、模板 GPUI/NDK 路径例外、同脚本额外未知路径仍 bypass 的回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/fmt/design docs/package list、diff check 通过；本机 Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含 `arm64-v8a`/`x86_64` |
| PR #320 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `822030c`；无版本发布/tag |
| 本地运行时验证（PR #322） | 未建模 `srcDir(...)`/`srcDirs(...)` bypass、模板 `gpui.jniLibsDir` source-root 例外回归通过；workspace 456 passed、1 个手动 benchmark ignored，集成/协议测试、clippy/build/fmt/design docs/package list、diff check 通过；Android debug/release packaging 与 CLI miss→hit smoke 通过，缓存 APK 含 `arm64-v8a`/`x86_64` |
| PR #322 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `dfc023c`；无版本发布/tag |
| 文档 PR #323 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `1475d24`；无版本发布/tag |
| 本地运行时验证（PR #325） | source-root setter、property assignment、Groovy command-style source roots bypass；注释/字符串不误触发；受控模板 JNI 例外仍通过；workspace 456 passed、1 个手动 benchmark ignored，clippy/build/fmt/design docs/package list、diff check 通过。未运行设备验收 |
| PR #325 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 最终全部通过；PR workflow 首次 macOS run 命中既有 registry manifest 测试偶发失败，按 job 重跑通过，fail-fast 取消的 Linux/Windows 也分别重跑通过；squash merge `1b765ee`；无版本发布/tag |
| 本地运行时验证（PR #327） | Groovy command-style `file/files`、`srcDirs files` bypass 与注释/字符串排除回归通过；workspace 456 passed、1 个手动 benchmark ignored，clippy/build/fmt/design docs/package list、diff check 通过；真实 Android debug/release packaging、`arm64-v8a`/`x86_64` ABI 与 debug/release CLI miss→hit smoke 通过 |
| PR #327 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `9c0fef8`；无版本发布/tag |
| 本地运行时验证（PR #329） | `File(...)`、`Paths.get(...)`、`Path.of(...)` 限定/非限定形式 bypass 与注释/字符串排除回归通过；clippy/build/fmt/design docs、package list、diff check 与真实 Android debug/release packaging、ABI、CLI miss→hit smoke 通过。Workspace 测试并行运行 452 passed、1 ignored、4 项既有并发/子进程测试失败；四项均单线程单独重跑通过 |
| PR #329 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `cc1da49`；无版本发布/tag |
| 本地运行时验证（PR #331） | `ClassLoader` resource APIs、`ServiceLoader.load`、`Class.forName` bypass 与注释/字符串排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs、Android debug/release packaging、ABI 与 CLI miss→hit smoke 通过 |
| PR #331 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `8a4d4b4`；无版本发布/tag |
| 本地运行时验证（PR #333） | `from(...)` / Groovy `from 'path'` file collection 与 source-set 调用 bypass；普通变量名、注释/字符串排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs、Android debug/release packaging、ABI 与 CLI miss→hit smoke 通过 |
| PR #333 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `be895ce`；无版本发布/tag |
| 本地运行时验证（PR #335） | `FileSystems.getDefault().getPath(...)`、`getCanonicalFile()`、`getCanonicalPath()`、`getAbsolutePath()`、`toPath()` 派生路径 marker 与限定/非限定形式、注释/字符串排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list、diff check 通过；真实 Android debug/release packaging、`arm64-v8a`/`x86_64` ABI 与 CLI debug/release miss→hit smoke 通过 |
| PR #335 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `e8d70cd`；无版本发布/tag |
| 本地运行时验证（PR #338） | Java `File` 元数据/权限查询、目录枚举和 mutation receiver-call marker bypass 回归通过；首次 smoke 暴露裸 `delete` marker 与模板 `Delete::class` clean task 冲突，收窄后 smoke 通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list、diff check、真实 Android debug/release packaging、双 ABI 和 CLI debug/release miss→hit smoke 通过 |
| PR #338 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `0e04377`；无版本发布/tag |
| 本地运行时验证（PR #341） | Java NIO `Files` 状态/属性、目录流、读写/变更、`FileSystems` store/root 和 `Path.toRealPath()` marker 回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list、diff check、真实 Android debug/release packaging、双 ABI 和 CLI debug/release miss→hit smoke 通过；首轮 push Windows 的既有 coordinator 时序测试失败后只重跑该 job 并通过 |
| PR #341 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；push Windows 失败 job 定向重跑后通过；squash merge `892fc21`；无版本发布/tag |
| 本地运行时验证（PR #345） | ZIP/JAR file/stream constructor 与 `FileSystems.newFileSystem(...)` marker 回归通过；注释/字符串、普通 app source 和模板 clean task 排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list、diff check 通过；真实 Android debug/release packaging 与 CLI debug/release miss→hit smoke 通过 |
| PR #345 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `70d7c5c`；无版本发布/tag |
| 本地运行时验证（PR #349） | `Scanner`/`PrintStream`/`PrintWriter` file-backed constructor marker 与限定/非限定类名、File/Path/String 输入、注释/字符串和普通 app source 排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging 与 CLI debug/release miss→hit smoke 通过 |
| PR #349 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `4d8b251`；无版本发布/tag |
| 本地运行时验证（PR #352） | `Path.toFile()` 限定/非限定 marker 与注释/字符串、普通 app source 排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过 |
| PR #352 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `7473a5b`；无版本发布/tag |
| 本地运行时验证（PR #355） | NIO watcher 创建、目录注册、事件读取 marker 回归通过，并覆盖注释/字符串、普通 app source、Gradle task registration/clean task 排除；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过 |
| PR #355 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `dd316eb`；无版本发布/tag |
| 本地运行时验证（PR #357） | `AsynchronousFileChannel`/`SeekableByteChannel` 限定/非限定类型与 factory marker 回归通过，并覆盖注释、字符串和普通 app source 排除；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过 |
| PR #357 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `90174a7`；无版本发布/tag |
| 本地运行时验证（PR #360） | `FileSystemProvider`、`DirectoryStream`、`SecureDirectoryStream` marker 及限定引用/使用、注释/字符串和普通 app source 排除回归通过；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过 |
| PR #360 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `550732e`；无版本发布/tag |
| 本地运行时验证（PR #363） | `Files.walkFileTree`/visitor/result/options/attributes marker 回归通过，覆盖 `FOLLOW_LINKS`/visitor callback、注释/字符串和普通 app source 排除；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI miss→hit smoke 通过 |
| PR #363 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `b7b1949`；无版本发布/tag |
| 本地运行时验证（PR #366） | `PathMatcher` 与 `FileSystem.getPathMatcher` 限定/非限定 glob/pattern 回归通过，并覆盖注释、字符串、普通 app source 排除；workspace 456 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check、Android debug/release packaging、双 ABI 与 CLI debug/release miss→hit smoke 通过 |
| PR #366 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过（首轮 Windows coordinator/mobile 时序用例短暂失败、fail-fast 取消 macOS job 后，按 job 定向重跑均通过）；squash merge `e0c36ed`；无版本发布/tag |
| PR #369 本地验证 | workspace 490 passed、1 ignored（CLI 单元测试 457 passed），fmt/clippy/build/design docs/package list/diff check 通过；Android debug/release packaging、双 ABI 和 CLI miss→hit 通过；同源码/同 BuildKey 的 `2.1.0`→`2.2.0` 环境插值由 `aapt2` 核对 APK 版本，两次 bypass、不发布 manifest，已有 manifests 保留 |
| PR #369 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 首次全部通过；squash merge `41a182e`；本轮未运行 GUI/设备验收 |
| PR #372 本地验证 | workspace 491 passed、1 ignored（CLI 单元测试 458 passed），fmt/clippy/build/design docs/package list/diff check 通过；Android debug/release packaging、双 ABI、CLI miss→hit 通过；同源码/同 BuildKey 下修改外部文件使 APK 版本由 `3.1.0` 更新为 `3.2.0`，两次 bypass、不发布 manifest，已有 manifests 保留 |
| PR #372 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 首次全部通过；squash merge `4d4f863`；本轮未运行 GUI/设备验收 |
| PR #375 本地验证 | stream wrapper API 引用/构造、限定与非限定形式以及注释/字符串排除回归通过；workspace 459 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check 通过 |
| PR #375 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `18c719a`；本轮未运行 GUI/设备验收 |
| PR #377 本地验证 | Properties `load`/XML load/store、opaque stream、其他 receiver/方法引用以及注释/字符串排除回归通过；workspace 459 passed、1 ignored，fmt/clippy/build/design docs/package list/diff check 通过 |
| PR #377 CI / 合并 | PR 与 push 两套 Linux/macOS/Windows、desktop-template、android-template、baseline-driver 全部通过；squash merge `852ddec`；本轮未运行 GUI/设备验收 |
| T05 release 对照 | Apple M2/macOS/aarch64、4096 files/32 MiB、热 filesystem cache、10 warmup + 30 alternating pairs；oracle mismatch=0；wrong_revision_acceptance=0；索引更新 P95 1.280 ms wall/1.242 ms process CPU，全量稳定 oracle P95 170.913/170.458 ms；单文件更新读 16 KiB，对照稳定双扫描读 64 MiB。只代表此主机和合成单文件变更 |

七项探针的关键结果如下。这些是无 GPU 的边界复现，不是完整 UI 场景验收：

| 探针 | 观察结果 | 本机记录 |
| --- | --- | --- |
| encoded Rust flags | 修复前探针曾在改编译参数后仍 cache hit；#149 已将变量纳入 allowlist，回归测试证明 encoded flag 改变 hash；尚未重跑合并后的 end-to-end CLI probe | `cli-probes.json`、[M04 encoded Rust flags](../experiments/M04-cargo-encoded-rustflags-2026-09-29.md) |
| check 会话归属 | 旧 preview 收到的 reset 数为 0，旧 preview 仍运行 | `check-probes.json` |
| check 清理 | 旧探针记录了 supervisor-only cleanup 的问题；#151 已改用 process group/Job Object，并用后代持有管道回归测试验证终止传播；尚未重跑完整 GUI check cleanup probe | `check-probes.json`、[S04 process-tree cleanup](../experiments/S04-process-tree-cleanup-2026-09-29.md) |
| fixture 身份 | 修复前探针记录初值 0→42、generation 1→2，报告 hash 仍为初始值；#153 已接入 runtime hash 刷新和报告传播，尚未重跑完整 GUI probe | `runtime-probes.json`、[S04 fixture identity](../experiments/S04-fixture-hash-2026-09-29.md) |
| 单场景 frozen check | #155 已让 desktop 单场景从重核验后的 workspace snapshot 读取 scenario/fixture，并在 context 中保留 snapshot hash/BuildKey；matrix 未纳入本切片 | [S04 frozen single check](../experiments/S04-frozen-single-check-2026-09-29.md) |
| matrix frozen inputs | #157 创建 shared snapshot，#159 写入 per-cell context，#161 写入 target-specific BuildKey，#163 绑定 source-project output layout 并串行同 key cell，#165 在 preview builder 取得该 output root 的跨进程锁，#167 保留 control scenario cell 的完整 CheckReport；仍未声称跨命令共享构建或真实三端 evidence | [S04 frozen matrix snapshot](../experiments/S04-frozen-matrix-snapshot-2026-09-29.md)、[S04 matrix context](../experiments/S04-matrix-context-2026-09-29.md)、[S04 matrix target BuildKey](../experiments/S04-matrix-target-build-key-2026-09-29.md)、[S04 matrix build output layout](../experiments/S04-matrix-build-output-layout-2026-09-29.md)、[S04 preview build output lock](../experiments/S04-preview-build-output-lock-2026-09-29.md)、[S04 matrix cell reports](../experiments/S04-matrix-cell-reports-2026-09-29.md) |
| preview output lock | preview desktop/iOS/Android build 在 source-project target-specific root 上通过持久 lock file 做跨进程排他；锁 guard 覆盖 preview build 过程并在进程退出/崩溃时由 OS 释放；只验证 lock 阻塞和 guard drop 释放，不声称命中或合并构建 | [S04 preview build output lock](../experiments/S04-preview-build-output-lock-2026-09-29.md) |
| matrix cell report | control scenario cell 的 `MatrixCellResult.check_report` 保留完整 steps、证据、primary error、cleanup 和 context，并通过 JSON round-trip 验证；executor 在 cleanup 后仍保留 runner context；unavailable/capture-only cell 不生成伪报告，但 capture-only lifecycle、runner metadata 与 event log 可见 | [S04 matrix cell reports](../experiments/S04-matrix-cell-reports-2026-09-29.md) |
| mobile scenario lease/evidence | #211 由 matrix supervisor 持有唯一设备 OS lease，preview 子进程只使用 owner path/session/token digest delegation；capture/native logs/stop evidence 绑定 preview 实际 run 并进入 `CheckContext.mobile_evidence`；#217 让 action operation 错误保留 operation ID 与 action status；#219 覆盖 pre-`scenario_ready` launch/registration failure；#221 记录 cleanup stop/release 错误并保证 stop failure 后仍释放 lease；#223 让 cleanup 后完成的 context/evidence 进入 `MatrixReport`；#225 写入 RunnerInfo/RunnerCapabilities；#227 保留已有 EvidenceLog 的生命周期事件序列；#229 将事件数量、detail 大小、淘汰诊断和 JSON 反序列化统一设为有界；#231 将 capture-only 的完整环境字段传播进 matrix context；#233 保留 prepare 阶段的 run identity 和 fencing token 摘要，原始 token 不进入 JSON；#235 让 control-driven scenario evidence 在 run 绑定后也提供嵌套 run identity，未绑定时保持 null；#237 让 capture event detail 复用有界 artifact/hash/viewport/scale/orientation/foreground 元数据，不写 host path 或 lease secret；#239 让事件级 project/lease/fencing digest 与同一 `RunIdentity` 对齐；#241 在两条移动截图证据路径发布前核验 PNG 完整性；#243 又发布并复核每张移动截图的 schema 1 sidecar manifest 与当前 run identity；unknown 仍为 inconclusive 且不重放；未声称真实设备矩阵已通过 | [M02 matrix contract](../experiments/M02-matrix-contract-2026-09-28.md) |
| Android capture environment | #213 将 Android display probe 的逻辑 viewport、scale、方向和保守前台包名写入 capture/check evidence；#231 又将这些字段连同 provider/path/hash/system UI 传播到 capture-only matrix context；#237 又把已取得字段写入 bounded capture event detail；#241 又在发布截图证据前核验 PNG 完整性；#243 又在 display metadata 补齐后原子发布并核验 sidecar manifest；adb 或厂商输出缺失时保持 unknown；未声称真实设备环境验收已通过 | [M01 Android runner](../experiments/M01-android-runner-2026-09-28.md) |
| desktop preview cache hit | preview 专用 manifest 绑定 desktop platform/BuildKey hash，逐文件验证内容并要求唯一 Cargo 可执行文件；命中跳过 Cargo，任何验证失败都 miss 并回退正常构建 | [T06 desktop preview cache hit](../experiments/T06-desktop-preview-cache-hit-2026-09-29.md) |
| iOS simulator live preview cache hit | preview 专用 manifest 绑定 iOS platform/BuildKey hash，逐文件验证 `.app` 内容并要求根正是当前 simulator bundle；命中跳过 rustup/Cargo/XcodeGen/`xcodebuild`，physical device 不命中 | [T06 iOS preview cache hit](../experiments/T06-ios-preview-cache-hit-2026-09-29.md) |
| Android default-debug live preview cache hit | preview 专用 manifest 绑定 Android platform/BuildKey/ABI，逐文件验证 JNI staging 与 debug APK 输出；default debug keystore 和 cache policy 仍有效时命中并跳过 rustup/cargo-ndk/Gradle | [T06 Android preview cache hit](../experiments/T06-android-preview-cache-hit-2026-09-29.md) |
| BuildKey output ownership | `BuildOutputLock` 持有 OS 锁后原子发布 `.build-owner.json`；owner_id 匹配时才删除，stale 记录在下一次成功加锁后覆盖，cache clean 不把 owner record 计入输出大小；不以 record 推断锁活跃性 | [S04 BuildKey output ownership](../experiments/S04-build-output-ownership-2026-09-29.md) |
| BuildKey coordinator | 普通 build/run 与 desktop、iOS simulator、Android default-debug/显式 debug custom-signing/release-only signing debug live preview/check 使用独立 attempt 类型和平台 manifest verifier；Windows 状态文件发布的瞬态 `PermissionDenied` 最多 5 次短退避重试，原子替换与 owner fencing 不变；physical iOS 与受控 Android local custom/release build/run 绑定签名输入摘要；Android unsigned release non-live build/run 也可复用 verified manifest；Android Gradle plugin signing markers 与 `includeBuild` 外部逻辑触发 cache bypass；release preview、复杂/远端 signing 的 Android preview 不发布可复用 manifest；preview follower 可在 superseded 时释放 subscriber，leader 消失可接管、失败/输入 supersession 可共享；复杂/远端 signing、physical live preview 和 cache-disabled 路径仍不共享 | [S04 BuildKey coordinator](../experiments/S04-build-coordinator-2026-09-29.md)、[S04 desktop preview coordinator](../experiments/S04-preview-build-coordinator-2026-09-30.md)、[S04 iOS preview coordinator](../experiments/S04-ios-preview-build-coordinator-2026-09-30.md)、[S04 Android preview coordinator](../experiments/S04-android-preview-build-coordinator-2026-09-30.md)、[T06 iOS physical signing](../experiments/T06-ios-physical-signing-build-key-2026-09-30.md)、[T06 Android signing](../experiments/T06-android-signing-build-key-2026-09-30.md)、[S04 preview coordinator cancellation](../experiments/S04-preview-coordinator-cancellation-2026-09-30.md) |
| Leader subscriber reference | coordinator leader 从发布 `building` 状态前持有 OS-locked subscriber，终态发布并返回后释放；failed attempt 在 leader 返回前仍被视为有活跃引用，follower 取消只释放自身引用 | [S04 leader subscriber reference](../experiments/S04-leader-subscriber-reference-2026-09-30.md) |
| Subscriber identity/count | subscriber 文件名绑定 attempt 的安全编码，active count 通过 OS lock probe 判定，不读取 locked JSON；failed-attempt sharing 按 attempt 过滤 | [S04 subscriber identity/count](../experiments/S04-subscriber-identity-count-2026-09-30.md) |
| v1 兼容 | app proto 1 为 `unsupported_version`；control schema 1 为 `invalid_schema`；schema 2 可用 | `cli-probes.json` |
| 历史升级 | 复制上次用 `3df7a6c` CLI 生成的未修改项目，执行 `upgrade plan --to agent-native-v1-draft --json`，仍退出 1、`baseline_unavailable` | `upgrade-probe.json`、`old-t02-upgrade.json` |
| doctor 版本/ABI/设备选择 | 历史 `runtime-probes.json` 记录了 exit 0 + 不可解析版本仍 overall=pass；PR #380 增加有效/最低版本、超时/大 stderr、SDK/NDK/Gradle/JDK 变体、required Rust target 缺失、iOS simulator selector 和 Android AVD ABI match/mismatch 证据，修复 Ubuntu `cc` marker 后 squash 合并；全平台 CI 通过 | [T01 doctor 验收摘要](../experiments/T01-doctor-closeout-2026-10-06.md)；CI runs `37436523210`/`37436528816`；CI 编译/单测通过不代替真实 Linux/Windows doctor host 矩阵；真实 x86/unknown ABI 与 physical device 仍未运行 |

fixture 探针直接编译当前 `previews.rs`，只用最小 env/report bridge 隔离验证 hash；
历史升级复用旧项目夹具，本轮没有重新编译旧 CLI。探针返回的预期失败不能统计为
工作包验收通过；原始脚本的退出码也不能替代逐项结果核对。

已有 Android capture-only 真实探针记录见
[M02 实验](../experiments/M02-matrix-contract-2026-09-28.md)：Counter 安装/启动/control/heartbeat/
设备 PNG capture 成功，1280×2856；同设备 semantics-required 场景为 unavailable。
PNG 仅有临时路径和记录 hash，未形成持久 CI 产物；#211/#223 修复该路径的 lease/run 归属与
cleanup-finalized report evidence 接线，
本轮没有重跑 GUI 或设备验收；#233 补充 capture-only 同一 run/lease 身份字段，#235 将同一边界
传播到 control-driven scenario evidence，#237 将已取得 capture metadata 写入事件序列。

## 6. 后续接续顺序

2026-10-06 纠偏：此前顺序将缓存隐藏输入和移动扩张放在基础兼容/验收收口之前。
现改为以下顺序，详细工作卡、阻塞切换和交接规则见[收口计划](closeout-plan.md)。

1. 先从 F01/T01 基础候选中按实际缺口与可用环境选择并依次收口；任务完成后自动推进下一项，外部等待须记录恢复条件并继续独立可执行工作，不是一次切片修复就标 done 或停止。
2. 按硬依赖收口 P01/T02/T03/F02，以及窗口/资源/升级/observe；补在线 v1 兼容、不可变历史基线并保留拒绝/降级边界，复核 O03 历史完成证据与 F02 依赖责任。
3. 收口语义/场景/租约/冻结输入和 macOS 三夹具连续 20 次及故障变体，再按依赖推进 MCP、移动完整语义/输入、三端矩阵及 repro；真实验收 driver 随任务补齐，不等 Q01 最后统一补。
4. T05/T06 剩余隐藏输入、复杂/远端 signing、release preview、预热和性能/Agent 基准保留为后续范围；当前 fail-closed 边界继续有效。已复现正确性/安全缺陷可有界插队，修复后返回原收口主线。

每次新合并先比较本文代码基线，再更新相关行、问题结论和验证记录。不得继续引用
`a6aa685` 的“matrix/Android adapter 未实现”或把旧 320 项测试当作新提交的检查结果。
