# 设计：Live 模式（改代码实时刷新运行中的应用）

状态：P0–P3 已实现（#4、#5、#6、#7）；P4 PoC 已执行——未通过，L2 不实现（见附录 C）
日期：2026-09-18
相关文件：`src/commands/run.rs`、`src/commands/build.rs`、`src/main.rs`、`src/template.rs`、`templates/app/src/lib.rs`、`templates/desktop/src/main.rs`

---

## 1. 背景与目标

当前 `gpui run` 的反馈回路是「一次性」的：编译、安装、启动，然后 CLI 退出（`src/commands/run.rs:465` 的 `handle_run`）。改一行 UI 代码要重跑整条命令，移动端还要手工重新导航回原来的界面。对 AI 驱动的开发流程而言这是最贵的一环——agent 写完代码后，要等一次完整构建才能看到结果。

本设计要解决四件事：

1. **监听与重建**：`gpui run --live` 监听源码变化，自动增量重建并重新加载到设备。
2. **错误获取**：编译错误、链接错误、运行时 panic、补丁装载失败——每一类都要能被 CLI 捕获、结构化展示，并在修复后自动恢复。这是本设计的一等公民，不是附属功能。
3. **通信通道**：CLI 与运行中的 app 之间建立一条双向通道，用于推送重载指令、回传日志与错误。
4. **分级重载能力**：明确哪些改动能走到「不重启」，哪些必须重启，并如实告知用户，而不是让用户对着没反应的界面猜。

推荐路线是 **L0 快速重启 → L1 资源重载 → L0.5 状态快照恢复**。状态快照恢复通过应用显式提供的可版本化数据保存/恢复接口，在重启后保留导航、表单和用户偏好；它比进程内热补丁更容易跨桌面、iOS 和 Android 保持一致。`subsecond` 只作为桌面侧实验性 PoC，不是 live 模式的交付前提，也不作为默认实现路径。

非目标：不做 GUI 前端调试器，不引入常驻后台服务（live 期间起的进程随 `run` 退出而退出），不支持 release 构建下的热重载。

---

## 2. 现状（设计的事实基础）

### 2.1 CLI 侧

`src/commands/run.rs`（488 行）已有可复用的构建与启动原语：

| 函数 | 行号 | 作用 |
|---|---|---|
| `run_desktop` | 141 | `cargo run -p <name>-desktop`，前台阻塞、继承 stdio |
| `build_ios_app` | 216 | `rustup target add` → `cargo build --lib` → `xcodegen` → `xcodebuild`，返回 `.app` 路径 |
| `run_ios` | 299 | 解析目标设备 → 启动模拟器 → 构建 → 安装启动 |
| `build_android_apk` | 367 | `cargo ndk` 产出 `.so` → `./gradlew assembleDebug`，返回 APK 路径 |
| `run_android` | 427 | 解析设备 → 启动 AVD → 构建 → 安装启动 |
| `run_step` | 110 | `Command::status()` 执行并检查退出码 |
| `handle_run` | 465 | 入口，按 target 分发 |

关键性质：**`install_and_launch` 已经是幂等的「重装 + 重启」原语**——iOS 模拟器走 `simctl launch --terminate-running-process`（`src/device/ios.rs:444`），真机走 `devicectl device process launch`（`src/device/ios.rs:461`），Android 走 `adb install -r` + `am start`（`src/device/android.rs:580`）。live 循环的每一轮迭代都可以直接复用它，无需新增启动路径。

### 2.2 通信与日志：完全空白

现有实现全部是 `Command::status()` 继承 stdio 的一次性调用，**CLI 与设备上的 app 之间不存在任何通道**：

- iOS 模拟器：`simctl launch` **未**传 `--console-pty` / `--stdout`，日志无管道。`src/device/ios.rs` 中 `capture`/`try_capture` 只用于一次性采集外部工具的 JSON 输出。
- iOS 真机：`devicectl process launch` 未带 console 参数。
- Android：**从不调用 `adb logcat`**；`install_and_launch` 之后 CLI 即退出。
- app 侧日志去向：Android 的 Rust 日志经 `android_logger` 进 logcat 但无人读取（见 §2.3）；iOS 无日志出口。

也就是说，**当前 iOS 真机模式下开发者完全拿不到应用日志**。这是一个独立于热重载的现存缺陷，live 模式的通道会顺带修掉它。

### 2.3 生成项目的形态

模板通过 `include_dir!` 编译期嵌入（`src/template.rs:8`），**改模板后必须重建 CLI**。生成结构：

```
<project>/
├── Cargo.toml                  # workspace: members = ["crates/app", "crates/desktop"]
├── gpui.toml                   # [app] [ui] [build] [run]
├── crates/app/                 # <name>-app，crate-type = ["lib","cdylib","staticlib"]
│   └── src/lib.rs               # MainView (impl Render) + {{MOBILE_ENTRY}}
├── crates/desktop/             # <name>-desktop，bin，main.rs
├── mobile/ios/                 # project.yml + App.swift + Info.plist
── mobile/android/gradle/      # GpuiActivity extends NativeActivity
```

两个对 live 模式有决定性影响的事实：

**事实一：教用户写的 UI 代码全部在 `crates/app`，而它不是任何平台的 tip crate。**

`templates/app/src/lib.rs` 里是 `MainView`；`templates/desktop/src/main.rs` 只有薄薄一层：

```rust
cx.open_window(WindowOptions::default(), |window, cx| {
    let view = cx.new(|_| MainView::new());
    cx.new(|cx| Root::new(view, window, cx))
})
```

**事实二：三个平台的「入口 crate」不是同一个。** 桌面是 `crates/desktop`（有 `main.rs`）；iOS / Android 是 `crates/app`——移动端入口 `gpui_ios_register_app` / `android_main` 由 `src/template.rs:176` 的 `mobile_entry()` 注入到 **lib.rs** 里，没有 `main.rs`。

**事实三：Android 已有 panic hook，iOS 没有。** `mobile_entry()` 生成的 `android_main` 里调用了：

```rust
gpui_mobile::android::jni::install_panic_hook();
```

而 iOS 的 `gpui_ios_register_app` 分支**没有任何 panic 处理**。运行时错误的可见性在两端不对称，这是 §7.4 要修的对象。

**事实四：模板没有任何 `[profile]` 或 `panic` 设置**（`templates/workspace.Cargo.toml`、`templates/app/Cargo.toml.template`、`templates/desktop/Cargo.toml.template` 均无）。dev profile 默认 `panic = "unwind"`，因此在明确包住的应用回调中可以讨论 `catch_unwind`；但这个默认是隐式的，需要显式固定。即使改成 `abort`，panic hook 仍会运行，只是随后进程终止，不能把 hook 当作恢复机制。

### 2.4 依赖现状

`Cargo.toml` **没有** `notify`、`tokio`、任何 WebSocket 库。已有 `serde` + `serde_json`（设备发现引入）、`anyhow`、`which`、`tempfile`、`colored`、`inquire`。

---

## 3. 行业方案调研

三种主流实现，机制完全不同，但指向同一条结论。

### 3.1 React Native Fast Refresh

**JS 天生解释执行**，所以 Metro 能增量编译单个模块并通过 WebSocket 推送到设备，运行时只重执行受影响模块。状态保留靠 React 的 reconciliation（Hook 调用顺序不变则 `useState`/`useRef` 值保留）。

其**错误处理**是本设计最该借鉴的部分：

| 错误类型 | 行为 |
|---|---|
| 语法错误 | 坏模块**根本不执行**，app 继续跑旧代码；红框显示，修复保存后自动消失，**永不重启** |
| 模块初始化期运行时错误 | 修复后 Fast Refresh 会话继续，模块重新注入 |
| 组件内运行时错误 | React 用新代码重新 mount；有 Error Boundary 则在下次编辑后重试渲染 |

局限：改了非组件导出、或文件被 React 树之外的模块引用，降级为全量 reload。

### 3.2 Compose HotSwan 2.0（2026-09-12 发布）

v1 受制于 JVM hotswap（只接受方法体内改动）。v2 的核心决策是**不跟运行时谈判，自带一个解释器引擎塞进 app**：JVM 加载的类永远不被重定义，结构性编辑（新增 composable、改控制流、整屏替换）由解释器直接执行。

| 变更类型 | v1 | v2 |
|---|---|---|
| 值编辑（颜色/间距/文本） | ✅ | ✅ 且**跳过编译，~5ms**（literal patching） |
| 增删 composable / 改控制流 / 整屏替换 | 部分/❌ | ✅（Android/Desktop 编译 1–2 秒） |
| iOS 模拟器 | ❌ | ✅，但结构变更需下发小型动态镜像，**10 秒以上**（官方承认延迟门未达标） |
| 状态/导航保留 | ✅ | ✅（Compose 把 UI 跟踪为 scope 树，只 recompose 受影响 scope） |

值得注意的定位信号：它提供 MCP server，让 agent「改文件 → 看运行中的屏幕 → 自我修正」闭环。

### 3.3 Dioxus：三层结构（最重要的一条）

Dioxus 0.7 把热重载拆成**三个完全独立的通道**，官方文档 `dioxuslabs.com/learn/0.7/essentials/ui/hotreload`：

| 层 | 机制 | 覆盖范围 | 要编译 Rust 吗 |
|---|---|---|---|
| **RSX 热重载** | RSX 解析器**同时在编译期和 devtools 中运行**，diff 后直接推给运行中的 app | 增删改元素/属性/样式/文本；甚至能把已有表达式在格式化字符串之间移动 | **完全不编译** |
| **Asset 热重载** | 监听 CSS/图片/字体，SCSS 自动重编译，Tailwind CLI 后台自动拉起 | 静态资源 | 完全不编译 |
| **Rust 热补丁**（subsecond，`--hotpatch`） | jump table，见 §3.4 | 函数体、逻辑、hooks | 编译变更代码 |

文档明确给出了 RSX 层的边界（即「不编译」能走多远）：新增上一轮编译中不存在的变量/表达式、RSX 之外的逻辑（函数体、hooks）、组件签名增删 prop、import 与模块结构、属性中涉及函数调用的复杂表达式——**这几类才需要 subsecond**。

而关键取舍写在限制章节里：**「RSX hot-reloading works across a workspace, Subsecond currently does not.」** 最便宜的一层做全 workspace 生效，最贵的一层限制在 tip crate。这个取舍本身就是设计答案。

### 3.4 subsecond 与 Rust 生态

**原理**：不改进程内存（不同于 `detour` 那类内存 patch），而是外部工具只编译变更部分，用运行中程序的函数地址链接出 **jump table** 发给 app，运行时把调用改道到最新版本。调用点用 `subsecond::call(|| ...)` 包裹。堆内存完全存活；patch 经 **Devtools WebSocket 协议**下发。仅在 `debug_assertions` 下启用，release 零开销。当前 crates.io 上可见的 0.8 系列仍带 alpha 标记；API、协议和构建约束不应视为稳定接口。

**限制清单**（官方文档）：

- **只补 tip crate**（`main.rs` 所在 crate）。官方原文警告：「Crate setups that have a `main.rs` importing a `lib.rs` won't patch sensibly」。原因是 rustc 构建图不确定 + 泛型转发会引起 codegen 级联。
- **struct 布局/对齐变化不支持**。框架作者需做 "re-instancing"；**Dioxus 的做法是直接丢掉旧状态整棵重建**。
- thread-local 在补丁后重置（官方标注为 HUGE WARNING）；static 初始化器变更不可见；全局变量可加但析构不执行。
- 需要 app 把 main 地址报告给 patcher（ASLR）。

`bevy_simple_subsecond_system` 的公开限制再次确认了两个事实：只补最顶层 binary，不支持 `lib.rs` 或 workspace；依赖结构变化和运行时类型布局变化仍需要冷启动。GPUI 当前模板正好是 workspace + `app` lib + `desktop` bin，因此这不是加一个依赖就能解决的问题。

**两个先例：**

- **Iced**（`iced-rs/iced` PR #3000，2025-06 合并）：把 subsecond 集成到**框架内部**（`iced/hot` feature），CLI 侧只是一个 `cargo-hot` 命令（`cargo run` 的 drop-in）。自述限制：「Very experimental! May crash your OS.」「Only changes to the root crate will trigger a reload.」「Changes to your application `State` or `Message` types will need a cold restart.」
- **hot-lib-reloader 0.8.2**：dylib 路线。函数必须 `#[unsafe(no_mangle)]` + 非泛型、签名与跨边界类型布局都不能变（变了即 UB/崩溃）、全局状态必须放主程序、macOS 上 dylib 需 codesign。**对 GPUI 不适用**——GPUI 组件大量使用泛型与 `Entity<T>`，画不出 `no_mangle` 边界。

### 3.5 推荐替代：状态快照恢复（L0.5）

对于 GPUI，最稳妥的状态保留方式是**重启进程，但由应用显式保存可恢复状态**，而不是在旧进程内替换 Rust 代码：

1. app 通过 live 专用接口注册 `save_state` / `restore_state` 回调，数据使用 JSON、MessagePack 或应用自己的版本化格式；CLI 只传递不透明字节和 schema id，不序列化 `Entity<T>`、窗口句柄或异步任务。
2. 重载前，CLI 发送 `prepare_restart`；app 在 UI 线程安全地生成快照，写入 `.gpui/sessions/<id>.state`，采用临时文件 + rename，避免半写入文件。
3. 构建成功后照常安装并启动。新进程通过启动参数、环境变量或平台专用临时文件获得 session id，恢复失败则丢弃快照并执行普通冷启动。
4. 快照带版本、大小上限和过期时间。schema 不兼容、文件损坏或应用未注册恢复接口时，CLI 必须明确显示“已重启，状态未恢复”，不能假装保留状态。

这条路径能在三端共享语义，保留导航、输入内容、主题和用户偏好等应用级状态，同时把 GPUI 的 retained tree、线程局部变量和原生句柄留在新进程中重新创建。它不能保留任意运行时对象，也不承诺“零闪烁”；收益是行为可验证、失败可回退。

### 3.6 方案选型结论

| 方案 | 适合范围 | 主要问题 | 本设计结论 |
|---|---|---|---|
| watcher + 重启 | 所有平台、所有 Rust 改动 | 丢失进程状态，移动端启动较慢 | **P0 默认基础** |
| watcher + 状态快照 + 重启 | 所有平台的应用级状态 | 需要应用显式注册 schema 和回调 | **P3 推荐路线** |
| `subsecond`/`whisker-subsecond` | 已验证入口布局的 debug desktop | alpha API、tip crate/workspace 限制、布局和 ABI 风险 | P4 桌面 PoC，默认关闭 |
| `hot-lib-reloader`/`hotload`/`relib` | 明确设计过 C ABI 的插件边界 | GPUI 泛型、`Entity<T>`、全局状态和签名约束不匹配；移动端还涉及 dylib 签名/加载 | 不采用 |
| `live-reload` 类 dylib wrapper | 主程序 + 独立动态库架构 | 要求把状态和所有跨边界类型移到 host，和当前模板结构冲突 | 不采用 |
| 只使用 `cargo watch`/`watchexec` | 简单重启 | 没有结构化诊断、设备安装、通道和快照协议 | 可作为调试工具参考，不作为 `gpui run --live` 实现 |

因此，“更好的 live 模式”不是寻找另一个可以直接替换 `subsecond` 的库，而是把重启循环做成可靠基础，再用 L0.5 解决最有价值的状态损失问题。只有在 PoC 证明 GPUI 当前入口布局可安全补丁后，才引入 L2。

### 3.7 生态给出的最重要信号：失败模式集中在错误可见性

不是功能本身，而是**错误可见性**。Iced 接入后最热的 bug 报告（`iced-rs/iced` issue #3146）是「Patches are getting ignored since there is no ASLR reference」——补丁静默失效，多楼讨论未收敛；另有案例是缺 `lld` 却由 `dx` 抛出无关错误。总结：

- 补丁**静默失效**、错误信息**指向错误的原因**，是这条路线上最高频的两个失败模式。
- 因此 §7.3 要求补丁装载失败必须**显式区分原因并自动回退到重启**，绝不静默忽略。

---

## 4. 关键约束：GPUI 拿不到最便宜的那一层

RN 的 JSX、HotSwan 的 Compose、Dioxus 的 RSX 有一个共同点：**UI 描述在运行时有独立的数据表示**——JS 对象、Compose scope 树、RSX AST。所以它们能绕过「替换已编译代码」，改为「替换数据」或「用自带解释器执行新代码」。

**GPUI 没有这一层。** 看模板：

```rust
div().flex().flex_col().items_center().justify_center()
    .child(div().text_2xl().child("{{APP_TITLE}}"))
```

这不是声明式数据经过编译器处理，**它本身就是 Rust 表达式**——UI 树只能通过执行已编译的 Rust 得到。

结论：

1. RN / HotSwan 的「自带解释器」在 GPUI 上没有便宜等价物。
2. Dioxus 的 RSX 层需要 GPUI 先有一个 RSX 式中间表示，**而它不存在**。
3. 因此 **GPUI 短期能稳定交付的上限是资源重载和状态快照恢复**；函数级补丁属于实验性能力，而不是 live 模式的基础假设。
4. 若要真正对标 RN 体验，唯一的路是在 GPUI 之上再造一个声明式层（等价于给 GPUI 写一套 RSX + 运行时 patch 协议）。这是**框架级项目，不是 CLI 能做的**，记为战略选项而非近期目标。

这也解释了为什么 Iced 的集成代码在框架侧而非 CLI 侧——**真正的工作量在 `gpui-kit`，不在 `gpui-cli`**。`gpui-kit` 目前是 git 依赖的独立仓库，因此 live 模式的最高形态是跨仓库项目，需要在实现前决定落在哪一侧。

---

## 5. tip crate 冲突：分平台互相打架

结合 §2.3 的事实一与事实二：

| 平台 | tip crate | 用户代码在哪 | 冲突 |
|---|---|---|---|
| desktop | `crates/desktop`（bin，有 main.rs） | `crates/app`（lib） | subsecond 会去补那个几乎不含用户代码的 crate |
| iOS / Android | `crates/app`（lib → staticlib/cdylib，无 main.rs） | `crates/app` | 没有 main.rs；且与 desktop 指向不同 crate |

后果：**「桌面热补丁生效、移动端没有」或反过来，是这个结构的必然而非 bug。**

三条候选路径：

| 路径 | 说明 | 风险 |
|---|---|---|
| (a) 重构模板，把用户代码放进 tip crate | 桌面可行 | 破坏共享 UI 的 lib 拆分；移动端入口本就在 lib 里，**无解** |
| (b) 在 `crates/app` 内加 dev-only `main.rs` 转发 | 可能绕开 tip 判定 | subsecond 官方已警告此结构「won't patch sensibly」，**必须 PoC** |
| (c) 等上游 workspace 支持 | 官方声明计划中 | 不可控 |

**这是 Phase 4 之前的决策关卡，必须先做 PoC 而不是直接投入实现。**

---

## 6. 分级重载能力与预期

明确区分能力等级，并在 CLI 中如实展示（§7.5、§7.6）。

| 能力 | 覆盖的改动 | 延迟 | 状态保留 | 崩溃风险 |
|---|---|---|---|---|
| L0 快速重启 | 任意 | 桌面 1–3s / 模拟器 5–15s | 无（进程重启） | 无 |
| L1 资源热重载 | 通过 dev asset source 加载的图片等资源 | 待测，目标 < 1s | 完整 | 低，需处理缓存失效 |
| L0.5 状态快照恢复 | 应用显式注册的导航、表单、偏好等状态 | L0 + 少量序列化时间 | 应用级状态 | 低，失败回退 L0 |
| L2 函数级热补丁 | 仅 PoC 验证过的函数体/逻辑 | 待测 | 不保证；类型/布局变化冷启动 | 高（见 §8） |

按平台：

| 平台 | L0 | L1 | L0.5 | L2 |
|---|---|---|---|---|
| 桌面 | ✅ | 试验性 | ✅ | 待 PoC，默认关闭 |
| iOS 模拟器 | ✅ 5–15s | 试验性 | ✅ | 不承诺 |
| iOS 真机 | ✅ 较慢 | 试验性 | ✅ | 明确不支持 |
| Android 模拟器 | ✅ | 试验性 | ✅ | 明确不支持 |

---

## 7. 方案

### 7.1 Phase 1：`gpui run --live` 快速重启循环（L0）

收益/成本比最高，是唯一不依赖任何未验证技术的一层。

```
notify 监听（去抖 300–500ms）
  → 判定改动落在哪个 crate
  → 增量 cargo build（--message-format=json）
  → 成功：install_and_launch（现有幂等原语）
  → 失败：不重启，保持旧 app 运行，渲染错误
```

要点：

- **desktop 需改造**：现在 `run_desktop`（`run.rs:141`）前台阻塞继承 stdio。live 模式下改为 spawn 子进程，变更时 kill + 重启，并接管其 stdout/stderr（这同时是 desktop 侧日志回传的基础）。
- **移动端零新增启动路径**：直接复用 `build_ios_app` / `build_android_apk` + `install_and_launch`。
- **去抖必须做**：编辑器保存常触发多次事件；`notify-debouncer-full` 是 `hot-lib-reloader` 的同款选择。
- **监听范围**：用 `cargo metadata --no-deps` 找到 workspace member，再加根 `Cargo.toml`、`Cargo.lock`、`.cargo/config.toml`、`gpui.toml`、iOS 的 `project.yml`/`App.swift`/`Info.plist`/资源目录，以及 Android 的 manifest、Gradle 脚本和 Java/Kotlin 源码。忽略 `target/`、`.git/`、`mobile/android/gradle/.gradle/`、`mobile/android/gradle/app/build/`、`mobile/android/gradle/app/src/main/jniLibs/`、iOS 生成的 `.xcodeproj` 和 build 目录；这些目录由构建工具写入，不能重新触发自身构建。
- **构建调度**：同一项目只允许一个构建/安装任务运行。构建期间到达的事件合并为一个 pending 标记，当前任务完成后只再构建一次；退出时取消 watcher、终止子进程并回收进程组，不能留下孤儿 desktop app。
- **模式限制**：`--release` 与 `--live` 互斥。live 必须明确使用 debug profile，避免把调试通道、快照协议或实验性 patch 带入 release 产物。
- **按键**：`r` 强制重建、`q` 退出（对标 `dx serve` 的 TUI）。用 `colored` 做行式输出即可，暂不引入 TUI 库。

预期：桌面 1–3 秒，iOS 模拟器 5–15 秒（`xcodebuild` 增量为主）。相比现状已是数量级改善。

### 7.2 Phase 2：dev server 与双向通道（L0 的错误闭环 + L1/L2 的地基）

Dioxus `dx serve`（TUI + devtools websocket + 资源 watcher + 自动拉起 Tailwind）印证了这是整套功能的地基，不是可选项。

```
CLI 侧：dev server；P1 默认使用带随机 token 的本机 IPC/loopback TCP，只有需要兼容 subsecond 协议时才考虑 WebSocket。
app 侧：dev-only 客户端（`debug_assertions` / feature = `live` 门控）。端口、地址和 token 必须通过启动参数、环境变量或平台专用配置注入，不能指望设备读取宿主机的 `.gpui/dev-port`。
```

双向用途：

| 方向 | 内容 |
|---|---|
| CLI → app | 重载指令；资源更新通知；未来下发 subsecond JumpTable |
| app → CLI | 日志转发；panic 上报；运行时错误上报；未来上报 main 地址（ASLR） |

**网络可达性**（各平台差异，需分别处理）：

| 目标 | 访问 host 的地址 |
|---|---|
| 桌面 | `127.0.0.1` |
| iOS 模拟器 | `127.0.0.1`（与 host 共享网络栈） |
| Android 模拟器 | `10.0.2.2`；优先用 `adb reverse` 把端口映射到设备 |
| iOS 真机 | 不默认假设局域网可达；USB/`devicectl` 隧道需要单独 PoC，无法建立通道时仍必须支持 L0 |

端口自动分配（bind 0 后读回）而非固定端口，避免多项目并行时冲突；port 文件写入项目的 `.gpui/`（加入 `.gitignore` 模板）。连接需要握手版本、project id、随机 token、超时和重连退避；通道断开不能阻塞主 UI 线程，也不能使 L0 重启循环失效。

顺带修复：**iOS 真机当前完全没有日志管道**（§2.2）——通道建立后，两端日志统一转发到 CLI。

### 7.3 Phase 2 的错误获取设计（一等公民）

按错误来源分层处理，**每类都要有明确的展示与恢复策略**：

| # | 错误来源 | 捕获方式 | 展示 | 恢复策略 |
|---|---|---|---|---|
| 1 | 编译错误 | `cargo build --message-format=json` 的 `compiler-message` 事件（`level == "error"`） | 终端渲染 `文件:行号:列号: 消息` + 代码片段 | **不重启，保持旧 app 运行**；修复保存后自动重建 |
| 2 | 链接错误 | 同上（cargo 会以 error 事件给出） | 终端 | 同上 |
| 3 | xcodebuild / gradle 失败 | 解析其 stderr；需要时用 `-json`（xcodebuild） | 终端 + 原始输出尾部若干行 | 不重启 |
| 4 | 运行时 panic | panic hook/平台日志回传（§7.4） | 终端 + 明确标记进程已不健康 | 进程退出后按 L0 重启；不承诺 panic 后继续运行 |
| 5 | 补丁装载失败 | channel 返回错误码 | 必须区分「未匹配 ASLR 引用」「跳转表版本不符」「布局不兼容」 | **自动回退到 L0 重启**，绝不静默忽略 |
| 6 | 设备/工具错误 | install/launch 退出码 | 终端 | live 循环暂停并提示，不反复重试 |

设计要点：

- **改用 `--message-format=json`**：cargo 会输出结构化 `compiler-message` 事件（含 span、level、rendered 字段）。这比解析 `stderr` 文本可靠得多，也让「错误 → 文件:行号」直接可跳转。`rendered` 字段自带你想要的彩色片段。
- **失败不重启**：第 1 类错误的处理哲学与 RN 一致（坏模块根本不执行）——保持旧 app 运行，用户能看到改动前的界面，修好即恢复。这是与「构建失败就杀掉 app」的关键区别。
- **必须区分「补丁没生效」和「补丁生效但代码没变」**：iced #3146 的教训（§3.7）。通道需返回显式的 patch 状态。
- **不吞错误**：任何一类失败都不允许静默继续。

### 7.4 运行时 panic 的可见性

现状不对称（§2.3 事实三）：**Android 已有 `install_panic_hook()`，iOS 没有。**

方案：

1. **模板侧**：desktop 与移动端都安装统一的 panic hook，把消息、位置和 backtrace 以有界、非阻塞方式送到 CLI；iOS 侧需要在 gpui-mobile 或模板中补齐实现。
2. **语义**：panic hook 只负责记录和上报，不能把 panic 变成可恢复错误。`panic = "unwind"` 允许在明确包住的应用回调内使用 `catch_unwind`，但不能跨 FFI、GPUI 事件循环或未知框架边界推广。
3. **显式固定 profile**：模板可显式写 `panic = "unwind"` 以保证开发构建的预期，但必须同时说明它不保证进程存活；`panic = "abort"` 也会执行 hook，随后终止进程。
4. **恢复**：CLI 收到 panic 或检测到子进程退出后进入 L0 重启策略。overlay 只有在单独验证过的应用层 `catch_unwind` 场景提供，不能列为 live 模式的通用能力。

### 7.5 Phase 3：状态快照恢复（L0.5）

这是默认的状态保留路线，前置于任何函数级热补丁。具体协议见 §3.5；P3 的验收重点是快照原子写入、版本不兼容回退、三端启动参数注入，以及恢复失败时不会阻塞冷启动。

### 7.6 Phase 4：subsecond 热补丁（L2）——仅实验性 PoC

**前置决策关卡（§5）**：必须先回答「`crates/app` 作为 lib 能否被 patch」和「移动端无 main.rs 怎么办」。因此 Phase 4 的第一步不是实现，而是 PoC：

**PoC 范围（建议）**：在一个生成项目里对 `crates/app` 的 `MainView::render` 做一次函数体改动，验证能否不重启生效；分别测 desktop 与 iOS 模拟器。若 desktop 通、移动端不通，就明确记录并只发布 desktop 的 L2。

**若 PoC 通过，实现要点**：

1. **模板改造**：在 GPUI 的渲染入口/事件处理点包 `subsecond::call(...)`。关键设计——**调用点的粒度决定状态损失的粒度**（subsecond 官方：「provide granular control over where patches are applied to limit loss of state」）。建议包在较细的单元（如单个 `render`）而非 `main`。
2. **release 零开销**：subsecond 仅在 `debug_assertions` 下生效，天然满足「release 不带热重载」。
3. **CLI 侧实现 patcher**：增量编译变更 crate → 用 app 上报的函数地址链接 JumpTable → 经通道下发。可参考 `dioxus-cli`（协议定义在 `subsecond-types`）与 `cargo-hot`（独立 `cargo` 命令，README 自述「Most of the code is taken from their `dioxus-cli` tool」）。
4. **类型和布局变化**：不尝试通过丢弃 Entity 树来掩盖 ABI 风险。struct、enum、trait、函数签名、静态初始化器、thread-local、跨 crate 依赖或无法分类的变更统一降级到 L0；补丁应用失败也必须回退并报告原因。
5. **明示限制**：tip crate 之外（即 workspace 其他 crate）的改动**不会**热更新，CLI 必须明确提示，不能静默。当前模板结构不满足 tip crate 约束，除非 PoC 先证明新的入口布局可行，否则不进入实现。

### 7.7 资源热重载（L1）

Dioxus 文档显示这是最便宜、确定性最高的一层：**不碰 Rust，只监听资源并在运行时重载**。GPUI 有自己的 asset/image 系统可挂钩。

首版只覆盖通过 dev-only 文件资源源加载的图片。`include_bytes!`、编译进 staticlib/cdylib 的资源不会自动变成可重载资源；字体还需要明确的缓存失效和旧字体释放策略。等图片路径验证完成后，再单独评估字体和样式资源。延迟目标暂定 < 1s，不能预先承诺零风险。

**性价比明确高于 subsecond**——建议在 subsecond 之前做，且在 subsecond 受阻时它是能独立交付的完整能力。

---

## 8. 风险与未决问题

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| 1 | **tip crate 结构冲突** | L2 在移动端可能完全不可用 | L2 仅实验性 PoC；不通就停在 L0–L1 + L0.5 |
| 2 | **移动端重启成本** | iOS 模拟器 5–15s，真机更慢 | 明确预期；L1 与 L0.5 优先，不能把 L2 当成提速承诺 |
| 3 | **GPUI 无 UI 中间表示** | 拿不到 RSX 那一层，天花板低于 Dioxus | 记录为战略选项（§4），不承诺短期实现 |
| 4 | **subsecond 生态成熟度** | alpha API、tip crate 限制、补丁静默失效和崩溃风险 | 默认关闭；桌面 PoC；显式 patch 状态；失败回退 L0；仅 debug |
| 5 | **模板编译期嵌入** | 改模板后不重建 CLI 则旧副本生效 | 沿用 `DESIGN-devices.md` 的既有提醒 |
| 6 | **跨仓库依赖** | 完整的 L2 需要改 `gpui-kit`（git 依赖的独立仓库） | 实现前先决定落地侧；否则 L2 只能停留在 CLI 侧 |
| 7 | Android 动态加载 `.so` 可行性 | L2 在 Android 未验证 | 明确不支持；Android 使用 L0/L1/L0.5 |
| 8 | iOS 代码签名 | 真机 L2 不可行 | 明确不支持；真机使用 L0/L1/L0.5 |
| 9 | **构建产物触发 watcher** | Android `.so`/Gradle 或 iOS 生成目录造成循环重建 | 明确忽略生成目录，并以“构建完成后空闲”作为验收条件 |
| 10 | **移动端地址发现** | app 读不到宿主机 `.gpui/dev-port`，真机网络还可能不可达 | 启动时注入地址/token；优先 `adb reverse`/USB 通道；通道失败不影响 L0 |

---

## 9. 依赖与改动清单

**新增依赖（CLI 侧）**：

| 依赖 | 用途 |
|---|---|
| `notify` + `notify-debouncer-full` | 文件监听与去抖（`hot-lib-reloader` 的同款组合） |
| `tokio`（或 `tungstenite` 同步版） | P1 dev server；仅在需要 subsecond 协议兼容时使用 WebSocket |

P1 可先用 `std::net::TcpListener` + 长度前缀 JSON 协议，避免为 L0/L0.5 引入异步运行时；协议需要握手版本、project id、随机 token、消息大小上限和 request id。待 Phase 4 确认需要兼容 subsecond 后，再引入完整 WebSocket。

**新增依赖（模板侧，dev-only）**：快照协议客户端（可用最小化手写实现以避免给用户项目引入重依赖）；`subsecond` 只有在桌面 PoC 通过后才加入，并应固定已验证版本。

| 文件 | 改动 |
|---|---|
| `src/commands/live.rs` | 新增：监听循环、去抖、重建编排、错误渲染 |
| `src/commands/error.rs` | 新增：`--message-format=json` 诊断解析与渲染 |
| `src/devserver/mod.rs` | 新增：本机 IPC/loopback 服务、端口分配、port 文件、握手认证 |
| `src/devserver/protocol.rs` | 新增：CLI ↔ app 消息定义（日志/panic/重载/patch 状态） |
| `src/commands/run.rs` | `run_desktop`（141）改为 spawn 模式；`run_ios`/`run_android` 抽出可复用的单轮迭代函数供 live 循环调用；`handle_run`（465）接 `--live` |
| `src/main.rs` | `Run` 增加 `--live`（及 `--no-live`）；`Commands` 视情况增加 `Dev` |
| `src/template.rs` | `mobile_entry()`（176）补统一 panic hook；生成 dev-only 通道和快照客户端；`vars_for` 增加 live 相关变量 |
| `templates/app/src/lib.rs` | 接入通道、快照回调与 panic 上报（dev 门控） |
| `templates/workspace.Cargo.toml` | 显式 `[profile.dev] panic = "unwind"`；可选 live feature |
| `templates/gitignore` | 加入 `.gpui/` |
| `templates/gpui.toml` | `[run]` 增加 live 相关默认值（如 `live_debounce_ms`、snapshot 限制） |
| `Cargo.toml` | 增加 `notify`、`notify-debouncer-full`、（后续）WebSocket 库 |

修改 `templates/` 后**必须重建 CLI**（`include_dir!` 编译期嵌入）。

---

## 10. 建议的实施顺序

| 阶段 | 内容 | 前置 | 价值 |
|---|---|---|---|
| **P0** | 快速重启循环 `gpui run --live` + 编译错误处理 | 无 | 立即数量级改善；零技术风险 |
| **P1** | dev server + 双向通道 + panic/日志回传（含修复 iOS 真机无日志） | P0 | 错误闭环；L1/L0.5 地基；独立修复现存缺陷 |
| **P2** | 资源热重载（L1） | P1 | 先交付 dev 图片资源；延迟目标 < 1s，需实测 |
| **P3** | 状态快照恢复（L0.5） | P1 | 三端共享状态语义；失败可回退 L0 |
| **P4** | subsecond 桌面 PoC | P1 | 决策关卡；默认不支持移动端 |
| **P5** | subsecond 实现 | P4 通过 | 实验性「不重启」；风险最高 |

**P0 一到两周可交付**。P1、P2、P3 即使最终放弃 L2 也独立有价值。P4 必须先做 PoC，再决定是否继续投入；没有 PoC 证据时不应把 L2 写成平台能力。

---

## 附录 A：外部命令与非本仓库代码速查

| 用途 | 命令 / 位置 |
|---|---|
| 结构化编译诊断 | `cargo build --message-format=json`（`compiler-message` 事件） |
| workspace 结构 | `cargo metadata` |
| iOS 模拟器日志 | `xcrun simctl launch --console-pty <udid> <bundle_id>` |
| Android 日志 | `adb -s <serial> logcat` |
| JetBrains 参考实现 | `compose-hotswan`（自带解释器；literal patching） |
| Rust 热补丁核心 | `subsecond` 0.7.10（jump table；仅 tip crate） |
| Rust 热补丁 CLI 参考 | `dioxus-cli`、`cargo-hot`（`hecrj/cargo-hot`，README 自述代码取自 dioxus-cli） |
| 框架内集成范例 | `iced-rs/iced` PR #3000（`iced/hot` feature + `cargo hot`） |
| dylib 路线（**不适用**） | `hot-lib-reloader` 0.8.2（需 `no_mangle`、非泛型，与 GPUI 冲突） |
| Dioxus 三层热重载文档 | `dioxuslabs.com/learn/0.7/essentials/ui/hotreload` |

## 附录 B：与初版结论的偏差

初版研究把 subsecond 当作主要路径，在看到 Dioxus 官方三层文档后修正：

- **Dioxus 的热重载不是一层，而是三层**（RSX / Asset / Rust hotpatch），而 **GPUI 因为「UI 即 Rust 表达式」而拿不到 RSX 那一层**（§4）。这是本设计最重要的一条结论。
- **tip crate 冲突比初判更严重且分平台**（§5）：三个平台的入口 crate 不同，冲突是结构性的。
- **资源热重载（L1）被提升为独立阶段**：它性价比高于 subsecond 且零崩溃风险，不应被当作 subsecond 的附属。
- **cargo-hot / Iced 的分工启示**：真正的工作量在框架侧（`gpui-kit`），CLI 侧只是命令包装——这使 live 模式成为跨仓库项目（§4、§8 风险 6）。
- **错误可见性是生态公认的最难部分**（§3.7），因此本设计把它当一等公民而非附属功能（§7.3、§7.4）。

## 附录 C：P4 PoC 执行记录（2026-09-19，未通过）

按 §7.6 的要求在实现前先做 PoC。环境：dx 0.7.10（dioxus-cli，本机源码安装）+ subsecond 0.7.10，在一个 `gpui init` 生成的三端项目（workspace：`app` lib + `desktop` bin）中，把 `MainView::render` 的函数体包进 `subsecond::call(|| ...)`，用 `dx serve --hot-patch --platform desktop --package <bin>` 驱动。

**结果：在到达 tip-crate 问题（§5）之前，就已在链接阶段被阻断，两种配置同一位置失败。**

1. 普通 hot-patch 构建：app 正常构建启动（`subsecond::call` 透传）。对 `crates/app`（lib，非 tip crate）做一次纯函数体编辑后，dx 正确检测到变更并进入 patch 流程，但 patch dylib 的部分链接失败——`libdeps-*.a` 中 `io_surface` crate 的目标码引用 `_IOSurfaceCreate/_IOSurfaceLock/...`，ld 报 `Undefined symbols for architecture arm64`。**app 进程存活、未重启**，dx 有明确错误输出（符合「不静默」要求，但功能不可用）。
2. `--fat-binary`（hotpatch 的官方前置要求）：dx 在生成 fat binary 时于同一位置失败（同一组 IOSurface 符号），app 从未被启动。

**根因**：dx 的部分链接步骤没有把 GPUI 依赖树的 framework 链接参数（`-framework IOSurface`，来自 `io_surface`/wgpu 一系）传递到 `libdeps` 归档的链接命令。GPUI 自身的正常构建完全不受影响——这是 dx patcher 与 GPUI 依赖树的兼容性问题，CLI 侧无法绕过。

**决策（按 §10 关卡逻辑）**：

- PoC 未通过 → **不实现 P5**。L2 记录为「实验性受阻」，`gpui run --live` 交付 L0、L1、L0.5。
- §5 的 tip-crate 问题（lib 能否被 patch、移动端无 main.rs）保持未决——在 dx 的 macOS 部分链接能处理框架依赖之前没有意义。
- 复现：任意生成项目 → `cargo add subsecond@0.7` → render 包 `subsecond::call` → `dx serve --hot-patch --fat-binary --platform desktop --package <name>-desktop` → 编辑 render 函数体。待 dioxus-cli 修复部分链接后可重跑此步骤作为关卡重测。
