# Android / iOS 分层 CI 切片

更新日期：2026-10-08；本地命令的实际 UTC 时间另存于 `environment.json`。

## 范围与基线

用户指定落实移动 CI 建议；代码基线 `2f85842`，包含 `f38d9df` 的 live diagnostics
修复及随后三份路线文档同步。PR #388 的 `49a6a3a` 是未合并的历史尝试，不继承其
未取得的 live ABI pass。新驱动复用相同验收意图，不要求先启动设备才能检查 cold inventory。

本地未提交运行以 `source.json` 的 revision、dirty、driver/workflow SHA-256 绑定；
hosted CI 还记录 `GITHUB_SHA`、run ID 和 attempt。没有新 CI run 前不得继承旧主线全绿。

## 实现责任

| 检查 | 责任与边界 |
| --- | --- |
| `doctor-android-cold` | 真实 SDK/NDK/JDK、x86_64 AVD 文件及镜像存在性；AVD 不启动，match 为 exit 0，ABI mismatch 为 exit 1 且只允许 selected-device required failure |
| `android-emulator (doctor)` | KVM 权限与 acceleration preflight；API 35/google_apis/x86_64 boot deadline 300s；`adb` 明确 serial、boot=1、实际 ABI；保留原 live match/mismatch 责任，不拿 cold 结果替代 |
| `ios-simulator (doctor)` | ARM64 Mac、指定 Xcode/runtime；本轮自建 stopped UDID 的 required-pass 与不存在 UDID 的 selected-device-only failure；删除自建 simulator |
| `android-native-build` / `ios-simulator (build)` | 独立完整 Debug native build，不以 emulator boot/Metal 为前置；初始依赖解析后保存 Cargo.lock；不能用 stub 打包或 cargo check 代替真实链接 |
| `android-emulator (smoke)` | 完整且未替换为 stub 的 GPUI APK；指定 serial 安装/启动、包 PID 连续五秒不变、PNG 完整性/哈希、包 PID logcat、crash buffer 和 uninstall |
| `ios-simulator (smoke)` | 链接 CoreGraphics 的宿主 Metal probe；完整 Rust+Xcode app、指定 UDID 安装/启动、PID+executable 连续五秒存活、PNG、PID native logs、shutdown/delete |
| `mobile-evidence-driver` | 不需要 SDK/设备的报告、phase/identity/ABI、无关失败排除、原始失败报告、超时、秘密筛除、PNG 与 runtime selection 回归 |

cold device 的 `state.kind` 必须为 stopped，live 必须为 running。Android serial 可以对应
具名 AVD，但必须匹配报告的 `state.serial`，不能把另一设备的同 ABI 当作通过。
负例必须只失败在所测试的 selected-device check，不能把缺 Rust target/JDK 等无关故障
算作成功的 mismatch。命令失败/超时在抛错前持久化；秘密 canary 检测失败并裁剪输出。

Android doctor 的 `android.cargo_ndk` 本来就是 required，因此 cold/live 两 job 均显式
安装 cargo-ndk 4.1.2，不假设 hosted image 已预装。完整 APK 的 cache manifest 根是目录，
不是 APK 文件名；driver 验证实际 Debug APK，manifest 存在时对照文件 SHA-256。
全局 Gradle init 等输入导致合法 cache bypass 时仍可验证 APK，但明确记录
`cache_manifest_available=false`，不把 native build pass 当作 cache acceptance。

## 验收层级与非目标

doctor 部分属于 T01 的 T-01/T-03 L1 responsibility；T01 无硬前置。完整应用 smoke 是
F01/P01 平台可行性证据设施，不能提前晋升依赖 F01+T01 的 P01 或其他移动父任务。
35 项父任务计数不变。

宿主 Metal device 不是 Simulator renderer/首帧证据；应用进程仍存活不是 ready 信号。
device screenshot 不保证目标应用前台或 app-owned pixels，因此 summary 明确记录
`verified_present=false`、`gui_acceptance=not_run`，smoke 还记录
`application_ready=not_instrumented`，iOS 的 `simulator_metal=not_instrumented`。
M-01/M-02 完整责任仍需截图归属、输入、键盘/方向/生命周期、early native crash、ANR、
进程重启/重用及 log attribution 变体；物理设备、unknown ABI、AGP/Gradle 边界也未关闭。

不修改上游 renderer、签名/许可，不接入公共 PR self-hosted runner，不增加未执行的
XCTest/instrumentation 用例，不用 artifact upload 成功或文档检查替代设备验收。
完整 Release native build 与 M-08 profile/ABI 并发矩阵不在本次 Debug smoke 出口。

## 本地与 CI 证据

本轮本地验证与 hosted CI 结果分别登记，不能从工作流配置推断已通过。
本机 Android 仅有 ARM64 system image，x86_64 live/KVM 责任由 Ubuntu CI 验证。
本机 iOS 有 26.2 runtime，driver 只创建/删除本轮 UUID 命名的 isolated simulator；
不 shutdown/delete all，不覆盖已有 Android 包。

### 本地结果

| 实际执行 | 结果 / 原始证据 |
| --- | --- |
| `python3 -m unittest discover -s scripts/tests -v` | 23 passed；报告/phase/identity/ABI、只有预期 required failure、原始报告先落盘、秘密泄漏裁剪、timeout、PNG CRC/解压、独立构建、failed-install cleanup、APK manifest 目录根和 cache bypass |
| `actionlint .github/workflows/ci.yml` / Python compile | passed；只是工作流/语法检查，不是 hosted/native acceptance |
| `cargo fmt --check` / workspace clippy `-D warnings` | passed |
| `cargo build --locked` | passed；本地 CLI 构建，不是移动运行时或 hosted CI 验收 |
| workspace test，默认并行 | 485 passed / 4 failed / 12 ignored；4 个既有 devserver/coordinator 时序测试在本机同时执行 native/boot 时失败；未改这些 Rust 测试或实现 |
| `cargo test --workspace --locked -- --test-threads=1` | 全部通过；主二进制 489 passed / 12 ignored，integration/protocol/xtask/doc tests 均通过；原命令输出 `/tmp/gpui-mobile-workspace-serial.log`。串行重跑不是 hosted 默认并行 CI 的替代 |
| PR #388 本地整合后 `cargo test --workspace --locked` | 默认并行完整 workspace 通过；主二进制 489 passed / 12 ignored，doctor/live/upgrade integrations、protocol/xtask/doc tests 均通过；日志 `/tmp/gpui-pr388-integration-tests.log`。23 个 Python 回归、Python compile、actionlint、fmt、design-doc 与 staged/unstaged diff checks 也通过；不覆盖 hosted 或设备 runtime |
| iOS cold doctor | `T-03/ios-cold/attempt-03`：iOS 26.2 owned stopped UDID required-pass / 不存在 UDID selected-device-only fail，shutdown/delete 后确认 UDID 不存在 |
| Android cold doctor | `T-03/android-cold-arm64/attempt-02`：API 35/google_apis_playstore/arm64-v8a AVD，match exit 0、x86_64 build ABI mismatch exit 1；本地 shell 删除本轮 AVD，原日志 `/tmp/gpui-mobile-avd-delete-02.log` |
| 完整 iOS Debug native build | `P-01/ios-build/attempt-02`：cargo lock 初始解析、完整 Rust staticlib 和 Xcode 链接成功，manifest 留存，owned simulator 删除；未启动/截图，不是 runtime/Metal acceptance |
| 完整 Android Debug native build | `P-01/android-build-arm64/attempt-01`：未修改为 stub 的 GPUI APK 构建成功，仅 `lib/arm64-v8a/libmobile_ci_probe_app.so`；APK SHA-256 `6012e0b5a875dc4f80a53c2a3d7441a5a17d107bcbc728d5f0ed7dcf8eaf0e66`，Cargo.lock、manifest、APK library list 和全部命令留存；没有 emulator runtime |
| iOS 早期 boot-first smoke | `P-01/ios-smoke/attempt-01`：bootstatus 300s timeout，末状态 Waiting on System App；summary=fail、raw boot stdout/stderr 保留，owned UDID shutdown/delete 后确认不存在。之后才分离独立 build 并调整为 build-first；新 runtime 流程未实际完成 |
| 宿主 Metal preflight | 本机 `xcrun swift -framework CoreGraphics ...` 返回 Apple M2；仅宿主能力，不是 Simulator renderer/首帧证据 |

上表的相对证据根是 ignored `artifacts/acceptance/2f85842-dirty/`，每次 source/driver/workflow
hash 以该 attempt 的 `source.json` 为准，不能将较早 driver 的 attempt 称作当前代码完整验收。
Android cold attempt-01 的 match 通过，但负例还遇到 required Gradle 5s timeout，driver
正确拒绝将其算作 mismatch pass；attempt-02 消除了该无关失败。iOS build attempt-01
因 fresh fixture 没有 Cargo.lock 被 CLI 的 locked metadata 拒绝；已在两个 native driver
加入初始锁文件解析和回归，而没有放松 CLI 的 locked 输入约束。

当前实现新加入的锁文件留存、PNG/manifest 验证等部分已有 offline regression；较早
local attempt 未记录的字段不补造。CI root evidence/preflight 目录加入 gitignore，避免它们
污染 `source.dirty` 或进入 source package。

### 等待与恢复条件

### 首轮 hosted 结果与 bootstrap 修复（`2eb7285`）

实际 push run `37711864318` / PR run `37711867631` 均已完成，overall=failure。
两套三平台 workspace checks、templates、baseline/mobile driver 回归和完整 Android
x86_64 Debug APK 构建通过；其余移动 jobs 失败，不能用这些局部通过晋升父任务。
push APK 含 `lib/x86_64/libmobile_ci_probe_app.so`，SHA-256 为
`e2283d092b9c4ac6851a1dd5cce0292ea90f8af9a1240d995fc0f536beaecb32`，
原始 `source.json` 绑定 revision=`2eb7285`、dirty=false 和 driver/workflow hash。

本轮有界缺口、修复与证据：

- Android live 两 jobs 已通过 KVM 权限/加速预检（`KVM ... installed and usable`），
  随后的 emulator version 命令因缺少 `libpulse.so.0` exit 127；未进入 emulator boot。
  在预检之前显式安装 `libpulse0`，记录 `ldd`；实际 emulator version 必须成功后才启动。
- Android cold 的 raw match report 只有 `android.selected_device` required/unknown：
  选中 stopped AVD，但没有 arch/runtime/image metadata；无法确认 ABI。旧 artifact
  没有 config.ini/实际默认 AVD 根，不能编造其确切路径。为创建器与 CLI 显式共用
  `ANDROID_AVD_HOME` 和 `--path`，留存实际 config.ini/list 及环境根；不手写 ABI。
- iOS doctor 的 required failures 是 `ios.xcodegen` unavailable 与
  `ios.rust_target.device` 缺 `aarch64-apple-ios`；build/smoke 也因找不到 XcodeGen
  失败。workflow 显式安装 XcodeGen 和 device/simulator 两个 Rust targets，
  保留工具链 preflight；不把 device target 检查降为 optional。
- 初始 run 的 raw logs/artifacts 已下载到 `/tmp/gpui-ci388-2eb7285/`；
  ignored 留存根为 `artifacts/acceptance/2eb7285/ci/push-37711864318/` 和
  `artifacts/acceptance/2eb7285/ci/pr-37711867631/`；
  27 项离线回归包含 bootstrap 配置契约与 AVD 根记录，但不是 Linux 库/模拟器验收。
- 本地使用真实 API 35/google_apis_playstore/arm64-v8a 镜像，以显式
  `ANDROID_AVD_HOME`/`--path` 创建 isolated AVD，cold match/mismatch 通过且 owned
  AVD 删除；实际 config.ini 与新版 environment roots 留存于
  `/tmp/gpui-ci388-cold-root-evidence-1791423453/`。这不是 hosted x86/live 验收。

本次仅修复已证实的 bootstrap/元数据可见性缺口；不修改 renderer、签名、doctor ABI
策略，不使用 continue-on-error，不宣称 cold/live/GUI 已通过。修复重跑后须核验
cold metadata/selected-device-only mismatch、iOS required tools 及实际 build/boot/process/
capture/cleanup artifacts。新修复的本地验证与 hosted 重跑结果须分别登记。
用户已授权提交推送此次修复；27 项回归、Python compile、actionlint 和文档/diff checks 通过，
没有修改 Rust 实现或重跑完整 native build，不借本地工具齐备推断 runner 修复通过。

### 第二轮 hosted 排障工作卡（`d9dc35b`）

2026-10-08，push run `37714516656` / PR run `37714519942` 已完成且整体失败。
两套 14 jobs 中只有 `ios-simulator (smoke)` 失败；Android cold/live doctor、完整
x86_64 APK、Android process/capture smoke、iOS cold doctor 和完整 Simulator build
及其他常规 jobs 均显示 success。job 成功不自动关闭 GUI/父任务责任。

本轮有界出口是从失败 artifact 定位 iOS boot 后 live doctor 的实际 required failure，
仅修复已证实的 driver/runtime 时序或代码缺陷，添加针对性离线回归，并保留失败报告
与清理证据。不降低 required checks、不吞失败、不先假设 Metal/renderer 已坏；不改
APK build 或其他已通过层。T01 无硬前置，smoke 仍不晋升 F01/P01/M01。

两套失败日志均显示 `doctor match exit was 1, expected 0`。push artifact 已核验：
完整 app build、bootstatus 和 owned UDID 清理均成功；boot 后的 live doctor 中
`rust.rustc`、`rust.cargo`、`rust.rustup`、`ios.xcodebuild`、`ios.simctl` 为 required/
unknown、`probe timed out`（约 5–8.5 秒），两个 required Rust targets 因 30 秒总
deadline 未执行。宿主版本探测在 boot 前仅需约 0.07–0.47 秒；这与 boot 后调度
压力一致，但报告不证明其底层原因。未进入 host Metal 或应用安装/启动，不可诊断为
renderer/Metal 失败。PR raw report 也已核验：上述 required timeouts 之外还含
`ios.xcodegen` timeout；`ios.simctl` 实测 duration=89922ms。两套 selected-device
均为 running/pass，清理成功。报告只证明 deadline failure，不证明宿主调度压力的
底层原因，也不证明配置的 5 秒/30 秒预算实现了硬截止。

针对该明确边界，已仅在 iOS live doctor 的每个用例增加至多两次、间隔 15 秒的 timeout-only
重探：必须是 schema/selector 身份正确、selected-device 状态符合用例、其余 required
failure 全为明确 probe/total deadline 超时；非零工具、缺工具、版本/target/selector
错误不重试。每次原始失败报告先持久化，不改 CLI 的 5 秒/30 秒预算；最终仍须完整
通过严格验证，否则失败。cold/Android 默认不重试；此修复不代表 GUI/首帧验收。
`doctor-retries.json` 保留失败报告引用/延迟；`doctor-result.json` 绑定最终通过严格验证
的报告，summary.live_doctor 记录使用的 retry 数，初次失败报告不会被覆盖。未恢复
时仍失败；额外工具退出/required failure 会直接停止，不把它算作 transient。

33 项离线回归已通过，新增恢复后失败报告留存、重探耗尽、非 timeout/错身份不重探、
负例 unrelated timeout 不算成功、默认/cold/Android 禁用重探、smoke 参数接线；
Python compile、actionlint 和文档/diff 检查通过。用户已授权本轮修复提交推送，真实 live iOS
重探恢复与后续 Metal/install/process/capture 仍待新 CI；没有修改 Rust probes/renderer。

push artifacts 的 Android cold/live match/mismatch 和 iOS cold selector 报告已由
同一严格 validator 重读通过；Android smoke 的 1080×1920 PNG 完整性/CRC/hash
与 summary 一致，package PID=4162、uninstall=pass；独立 Android/iOS build=pass，
所有 source revision=`d9dc35b`、dirty=false。仍不证明前台归属、app-owned pixels 或 GUI。
原始 downloaded artifacts/logs/jobs 留存在 ignored
`artifacts/acceptance/d9dc35b/ci/push-37714516656/` 与
`artifacts/acceptance/d9dc35b/ci/pr-37714519942/`；PR 存储 iOS smoke raw artifact/
jobs/logs，不补造尚未下载的其余 PR artifacts。

### 第三轮 hosted 排障（`82a353c`）

push run `37719062475` 的 14 jobs 全绿；PR run `37719065008` 的 14 jobs 仅
`ios-simulator (smoke)` 失败。其日志含三个普通工具 timeout、
`ios.rust_target.simulator` 的 `Rust target probe timed out` 和 device target 的
total deadline。现有 classifier 只接受普通 probe 与 total deadline 两种原因，漏掉了
Rust target 专用原因，因此错误标签仍是 `match`，没有进入重试。
该 `unknown` 结果的 `installed=false` 不证明 target 缺失；确实缺失时 CLI 返回
`fail` / `Rust target ... is not installed`，必须保持直接拒绝。

本轮仅补齐明确 timeout 分类，不修改 5s/30s 预算、required 策略或两次重试上限。
回归先复现正负 selector/simulator/device target 全部提前失败和零次重试；修复后
36 项 driver 测试通过，涵盖恢复后严格验证、逐次原始报告保留、持续超时失败，以及
缺 target/缺 rustup/非零退出/启动错误混合普通 timeout 时仍不重试。
Python compile、actionlint、design-doc 与 diff checks 通过；这是本地证据，修复后
hosted 结果须另行绑定新 run/job/artifact。T01/F01/P01 和 GUI/真机责任不晋升。
本轮 `cargo fmt --check`、workspace clippy `-D warnings`、默认并行完整 workspace
test（主二进制 489 passed / 12 ignored，integration/protocol/xtask/doc tests 通过）和
`cargo build --locked` 通过；本地测试日志 `/tmp/gpui-ci388-rust-target-workspace.log`。

两套 iOS smoke 的选定原始证据已下载到 ignored
`artifacts/ci/37719065008/ios-smoke-selected/` 和
`artifacts/ci/37719062475/ios-smoke-selected/`（JSON、boot/process 输出及成功 PNG，
未下载完整 native diagnostics 日志）。PR source 为测试合并提交 `188d701`、dirty=false，
build/boot/cleanup 通过，`commands.json` 确认仅一次 match；将其原始报告交给修复后的
classifier 可进入有界重试。push source=`82a353c`、dirty=false，match 第一次超时、
重试一次后通过，unknown UDID 只触发 selected-device failure；host Metal、安装、启动、
PID=36244 的进程采样与 capture 后检查、owned UDID 清理均通过。1179×2556 PNG 的
CRC/像素尺寸/hash 已重验，SHA-256=`62f0839a2d6bb6bf6c7eaa20b3cf856f6966bf38340eaf5687329a53d224e967`。
该成功仍标记 `verified_present=false`、`gui_acceptance=not_run`；不能代替修复后 PR CI。

### 发布与后续验收

`2eb7285`/`d9dc35b` 两轮 hosted 结果已如上核验；live iOS timeout-only 修复尚待新 run，GitHub PR 尚未合并。2026-10-08 已在
现有 `codex/t01-android-x86-abi-evidence` 分支（整合起点=`49a6a3a`）本地合入
`origin/main`=`2f85842`，三份路线文档冲突已解决。主线的 live
diagnostics 测试修复及历史失败证据保留；旧 `doctor-android-emulator` job 和
`scripts/doctor-android-evidence.py` 由新的分层 jobs/driver 替代，不再并行运行旧检查。
原工作区另有本地备份和 stash，用户已明确授权提交推送；发布状态以 Git/GitHub 为准，
下一动作是收集并核验本次提交对应的新 CI，不将授权或推送本身作为通过证据。
Ubuntu x86_64 cold/live、x86_64 完整 APK、hosted ARM64 Mac cold/native build 已取得
上述证据；live iOS 的最终 doctor/Metal/process/capture 仍须绑定新实际 run/job/artifact。
不将本机 ARM64 APK 或 cold AVD 结果扩展为 x86 runtime 通过。

本地代码/回归/独立构建责任已登记，runtime 和父任务仍等待上述 CI 与真实变体。
CI 恢复后的第一动作是逐层核对失败报告和实际 PID/ABI/capture/cleanup，再决定补 renderer
ready/verified present、前台归属与输入/崩溃变体；不是将工作流“存在”标作 done。
