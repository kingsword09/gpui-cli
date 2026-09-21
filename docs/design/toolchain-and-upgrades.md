# 设计契约：工具链、开发支持库、模板升级与构建效率

状态：T01 的规则、probe 和 CLI 已实现；T02–T06、F02 仍拟议。基线：`6d091b6`。

## 1. 工程基础的目标

工具链错误应在构建前定位，旧项目应能知道自己缺少哪些新能力，优化后的扫描/缓存必须保持版本判断正确。本设计支撑 [观察](observation-protocol.md) 与 [矩阵](platform-matrix-and-repro.md)，不改变其成功判定。

## 2. Doctor v2：检查实际可用性 T01

当前命令（T01）：

```bash
gpui doctor --target desktop --json
gpui doctor --target ios --sim "iPhone 17 Pro@26.2" --json
gpui doctor --target android --device emulator-5554 --json
```

未指定 target 时读取项目已选择的平台；不在项目内则生成 host-only desktop 报告。只有选中目标的 required检查失败才导致非零退出；可选工具缺失单列 warning。T01 的报告和 probe 已经共享 schema-v2 模型，完整平台矩阵仍需后续验收。

### 2.1 检查项

| 区域 | 必查内容 |
| --- | --- |
| Rust | rustc/cargo/rustup 命令成功、实际版本、host、所需target、项目rust-version |
| Desktop | 当前OS支持、C工具链、GPUI对应原生依赖、图形会话是否可用 |
| iOS | macOS、Xcode路径/版本、SDK/runtime、xcodegen、目标设备、签名是否需要/已配置 |
| Android | SDK/NDK目录有效且版本可读、cargo-ndk、目标ABI、JDK版本、平台SDK、build-tools、adb及设备状态 |
| Live runtime | 模板/runtime/协议版本、旧模板缺少的观察能力 |
| Capture/profile | 当前backend、截图权限、可选外部工具；未提供时不阻塞普通构建 |

仅 `which` 找到文件不等于工具可用。每个检查记录 command名称、退出码、已筛选的错误摘要、探测时长和实际版本。单检查默认5s，总预算30s；超时为 unknown/error，不阻塞无限久。

JDK要求来自项目使用的Gradle/AGP组合，Rust target来自选定ABI；不能写死“Android始终只需aarch64”或无条件要求全部移动工具。

### 2.2 报告 schema

报告保留 `schema_version:2`、project/target、overall、checks数组。每项字段为 id、required、status、expected、actual、reason、remediation、duration_ms。

`status = pass | fail | warning | unavailable | unknown`；修复建议包含可读原因和参数数组形式的建议命令，不是自动执行的shell脚本。敏感环境只报告是否存在/来源，不打印值。

退出码：0表示选中目标的required条件满足；1表示不满足/未知；2表示参数错误。MVP只诊断，不新增自动下载、许可接受、签名或系统配置修改。

## 3. 开发支持库与协议分离 F02

### 3.1 拆分原则

当前 `templates/app/src/live.rs` 内嵌协议、网络、日志和资源行为，每个生成项目都有一份副本。新方案拟议：

- `gpui-dev-protocol`：共享类型、编解码、版本和能力；不依赖GPUI/window/platform。
- `gpui-dev-runtime`：应用侧生命周期、UI队列、窗口/场景/日志适配；GPUI相关接口位于单独adapter模块。
- CLI继续持有构建/设备/文件系统/报告；runtime不能获取任意宿主路径访问能力。

先抽取当前行为并保留v1，再加入v2；不要用协议重构同时改变状态恢复语义。

### 3.2 Feature 和 release 边界

拟议生成应用feature：`gpui-dev`用于Live观察/交互，`gpui-profile`用于优化构建下的受限测量/场景支持。CLI在合适目标上显式传feature；普通项目构建不被迫开启监听端口。

Cargo不能用 `[target.'cfg(debug_assertions)'.dependencies]` 可靠选择debug依赖。必须用真正的optional dependency/feature，加Rust cfg门控来定义行为。

- debug Live显式启用gpui-dev；runtime初始化同时检查cfg和启动凭据。
- 正常release不启用gpui-dev；在 `not(debug_assertions)` 且启用gpui-dev时使用明确的编译错误。gpui-dev与gpui-profile同时启用也报错。自定义优化构建应使用独立gpui-profile，不通过打开debug_assertions绕过边界。
- gpui-profile保留最小采样/场景控制，默认不注册任意输入/属性修改工具；与生产release明确区分。
- 不能为获取截图无条件给所有项目启用GPUI test-support。先验证feature传播与体积/依赖影响。

### 3.3 开发期与发布期依赖

开发期使用workspace路径依赖和测试专用依赖覆盖。用户生成项目不得引用CLI开发者机器上的绝对路径。

发布前必须使用已发布的crate版本，或真实存在且不可变的Git revision。发布顺序：protocol → runtime → CLI/模板；每一步验证 `cargo package`、依赖解析和生成项目构建。

版本兼容矩阵记录CLI/API、runtime/proto、GPUI family、gpui-kit/gpui-mobile revision和模板版本。生成项目锁定兼容组合；自动升到所有依赖最新版本不属于升级策略。

## 4. 模板基线记录 T02

新项目生成 `.gpui/template-manifest.json`，记录generator/template版本、选定平台、依赖组合以及每个受管理文件的初始SHA-256和逻辑组。示例：[template-manifest.json](../examples/template-manifest.json)。

manifest是升级元数据，不包含dev-token和设备凭据。它与应用代码一起版本管理；需要调整模板gitignore，使这一文件可跟踪，而Live输出继续忽略。

只记录hash不足以做三方合并。CLI还必须能按template_version读取确切基线内容：从内嵌历史模板、内容寻址缓存或版本包取得，并校验发行摘要。缺基线时返回 baseline_unavailable。

旧项目没有manifest时，先用已知模板版本进行识别。仅在受管理文件足以唯一匹配时生成建议基线；无法唯一识别时输出manual_migration_required，不能把当前用户文件伪装成原始模板。

原子逻辑组至少包含：Android renderer vendor目录与workspace patch、app/desktop入口与feature、iOS linker/host配置、协议runtime依赖。它们不能部分升级后被报告成功。

## 5. Upgrade plan T03

拟议命令：

```bash
gpui upgrade plan --to <template-version> --json
gpui upgrade apply --plan <plan-id> --json
gpui upgrade recover --transaction <id> --json
```

plan只读。输入为base模板B、当前文件L、目标模板N。输出plan_id、目标版本、所有输入hash、每个操作及冲突、所需工具链变化和验证命令。

三方规则：

| 条件 | 行为 |
| --- | --- |
| L=B，N改变 | 可直接替换为N |
| N=B，L改变 | 保留L |
| L=N | 无操作 |
| L与N均不同于B | 可证明的结构化合并或标冲突 |
| 新文件目标已存在 | 相同则无操作，不同则冲突 |
| 上游删除、用户未改 | 提议删除并备份 |
| 上游删除、用户已改 | 冲突，不能直接删除 |

首版允许明确字段级别的TOML变更和未修改文件替换；不承诺自动合并任意Rust/Swift/Java源码。TOML编辑使用保留注释/格式的结构化编辑器，不能反序列化整个文件后丢弃用户注释。

Android ABI修复是必须包含的迁移夹具：旧hard-coded abiFilters、自定义多ABI列表、用户自定义release签名、renderer patch四种情况都有期望结果。

## 6. Upgrade apply 与恢复 T04

apply只能消费一个无未解决冲突、hash仍匹配的plan。显式执行apply意味着调用者选择执行这份计划；工具不在后台自动升级项目。

执行顺序：获取项目升级锁 → 重核所有输入与symlink → 写transaction journal和备份 → 准备全部临时输出 → 逐文件验证并原子替换 → 更新manifest → 运行指定验证 → 标记完成。

文件系统不提供整个项目的多文件原子提交。必须实现可恢复事务：

- journal记录每个文件old_hash/new_hash/备份/写入状态。
- 每次替换前再次检查当前hash；发生并发修改立即停止。
- 自动回滚仅恢复当前hash仍等于本事务写入值的文件，不能覆盖用户在失败后做的新编辑。
- 杀进程/断电模拟后，下次命令报告未完成transaction，允许inspect/recover；不把半升级状态当成新基线。
- 验证失败时保留备份、报告和日志；恢复路径使用精确记录的相对文件，不执行宽泛删除。

验证最小集：manifest可解析、模板依赖解析、所选平台源代码检查；native打包/运行是否执行由plan明确列出。没有工具链的验证为not_run，不能宣布所有平台升级通过。

## 7. 输入索引 T05

优化目标是降低重复全量哈希成本，同时不削弱observe/matrix的版本保证。

### 7.1 索引结构

按规范化根/路径记录内容hash、大小、mtime、文件身份、输入类别及依赖来源。mtime只是缓存提示，不作为唯一正确性依据。watcher事件将路径标dirty；rename和delete需要更新旧、新路径。

使用cargo metadata与原生宿主清单明确输入范围。README等文件是否参与构建取决于项目的build脚本/嵌入行为；不能简单全局忽略任意扩展名。

### 7.2 正确性策略

- 正常保存：仅哈希dirty文件和必要父目录，合并事件。
- `observe --sync`：进行完整一致性核验，或使用已证明完整的事件日志；第一实现保留全量核验。
- watcher溢出、目录链接、网络文件系统或索引异常：回退扫描，标明scope/成本。
- 冻结矩阵：构造独立不可变快照，不能只复用普通Live的索引结论。
- 检测文件读取期间变化并重试；连续变化返回superseded/unstable_inputs。

验证必须包含保持size/mtime但改变内容、原子替换、外部path dependency、符号链接循环、事件丢失和目录重命名。性能提升不允许通过漏掉输入实现。

## 8. 构建缓存与预热 T06

遵循矩阵设计的BuildKey；每种compiler/profile/ABI/native配置隔离。缓存manifest校验所有产物的存在、大小和hash，不能仅看“目录存在”。

### 8.1 输入与环境键

输入清单按规范化项目相对路径排序；外部依赖使用显式 external_root_id 加相对路径。每条含 kind、path、size、content_sha256，包含是否可执行等影响构建的 metadata；不包含 mtime。采用版本化规范 JSON（UTF-8、对象键排序、无多余空白）进行域分离 SHA-256，分别产生 source/asset/input/build_key。路径大小写或 Unicode 归一后碰撞必须拒绝，不能把两个文件悄悄合并。CLI/runtime 共用类型和测试向量。

| 环境类别 | 初始需纳入的输入 | 输出/秘密策略 |
| --- | --- | --- |
| Rust编译 | RUSTFLAGS、CARGO_ENCODED_RUSTFLAGS、RUSTC_WRAPPER、RUSTC_WORKSPACE_WRAPPER、选定target的CARGO_TARGET_*_LINKER/RUSTFLAGS | hash实际值和wrapper版本；不输出完整环境 |
| C/native | CC/CXX/AR、CFLAGS/CXXFLAGS、SDKROOT、部署target、选定NDK/SDK/JDK、Gradle参数 | 记录解析后工具路径标识/版本；不同有效工具链使缓存失效 |
| GPUI宿主 | GPUI_ANDROID_ABIS、renderer/backend feature、项目声明的影响编译变量 | ABI/profile与native manifest共同入键 |
| 项目自定义 | 显式声明的build.rs/proc-macro读取变量和外部文件 | 不能自动发现任意代码I/O；未声明时标范围不完整 |
| 签名 | 证书身份/配置版本等非秘密标识 | 不把密码/私钥写入manifest；签名产物不跨身份共享缓存 |

PATH本身不直接等同工具链：记录实际解析到的工具及版本，包装器内容变更也应失效。普通artifact不因未影响编译的设备serial失效，但安装/运行必须新建身份。用户声明的秘密环境可以影响构建却不能明文导出；无法建立稳定安全缓存键时禁用该缓存项。源码快照是确定输入集合，不是执行任意build脚本的完整沙盒保证。

### 8.2 缓存行为

- 相同BuildKey的在途任务共享结果；并发请求不重复cargo/Gradle安装。
- failed/cancelled/partial产物不进入可命中缓存。
- 工具链版本、锁文件、build脚本、features、影响编译的环境改变均失效。
- native构建缓存可以复用，但安装/运行身份必须每次重新建立。
- 预热只在显式开启时执行，有CPU/内存/磁盘预算；编辑中的前台构建优先。
- 先测量Cargo自有缓存命中；sccache等作为可选provider，不把其安装作为基本功能前提。

报告需要说明命中/未命中原因和节省的阶段，不以估算代替实际耗时。清理按缓存索引和引用计数，不删除正在使用的二进制或JNI输出。

## 9. 验收与发布

T01对应T-01–T-03；T02/T03对应T-04/T-05；T04对应T-06/T-07；T05对应T-08/T-09；T06对应T-10。F02同时执行协议兼容与release边界用例。

发布说明必须区分“升级CLI可获得的功能”和“应用需要迁移runtime/模板才可获得的功能”。升级命令不能把新版本号写进manifest后就宣布迁移完成。

具体步骤与PR拆分见 [实施清单](../roadmap/implementation-backlog.md)，故障输入见 [验收矩阵](../roadmap/acceptance-matrix.md)。
