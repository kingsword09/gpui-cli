# 设计：模拟器与真机的发现、选择、启动（`gpui device`）

状态：草案（待评审）
日期：2026-09-17
相关文件：`src/commands/run.rs`、`src/commands/build.rs`、`src/commands/doctor.rs`、`src/main.rs`

---

## 1. 背景与目标

目前 `gpui run ios` / `gpui run android` 的设备选择逻辑是隐式的：硬编码一个模拟器名字、从文本输出里猜 UDID、取 `adb devices` 的第一行。用户既看不到机器上装了哪些模拟器和真机，也无法指定启动哪一个，更不能新建模拟器。

本设计要解决四件事：

1. **发现**：列出已安装的 iOS 模拟器、iOS 真机、Android AVD、Android 在线设备，并带上足以区分彼此的版本信息。
2. **选择**：让用户在命令行（交互式或非交互式）挑一个，或在 `gpui.toml` 里固化默认值。
3. **启动**：按平台把选中的设备启动到可用状态，而不是假设它已经开着。
4. **创建**：回答"是否用新模拟器"——按指定版本/镜像/机型创建 AVD 或 Simulator。

非目标：不做 GUI 设备管理器，不接管 `xcodebuild`/Gradle 的构建流程，不引入常驻后台服务。

---

## 2. 现状问题

现有实现集中在 `src/commands/run.rs`，其设备选择部分是脆弱的：

| 位置 | 问题 |
|---|---|
| `simulator_name()` | 硬编码 `"iPhone 16 Pro"`，无运行时（iOS 版本）概念 |
| `resolve_simulator()` | 用 `line.contains(preferred)` 做文本匹配；名称在不同 runtime 间会重复，命中结果取决于输出顺序 |
| `extract_udid()` | 靠括号内字符集"猜"UDID，非结构化解析 |
| `adb_target_kind()` | 只取 `adb devices` 第一个 `device` 行；后续 adb 命令从不传 `-s`，多设备在线时不可控 |
| `run_android()` | 只对模拟器打印一条渲染警告就继续，不会启动模拟器，也不会创建 AVD |
| `doctor.rs:56` | 只检查 `adb`；`emulator`、`avdmanager`、`sdkmanager` 未检查，`xcrun simctl`/`devicectl` 未检查 |

`src/main.rs` 的 `Commands` 枚举（第 45 行起）目前只有 `Init` / `New` / `Doctor` / `Run` / `Build` / `Info` / `Completions`，没有设备相关子命令，`Run`/`Build` 也没有 `--device` 一类的标志。

---

## 3. 本机实况（设计的事实基础）

以下为 2026-09-17 在本机实测的结果，设计中的每一条外部命令都以此为准。

### 3.1 iOS

- Xcode 26.2 (17C52)，`xcrun devicectl` 506.6。
- 两个可用运行时：`iOS 18.4`、`iOS 26.2`。
- 22 台可用模拟器（每个运行时 11 台），探测时全部为 `Shutdown`。
- **名称冲突实例**：`iPhone 16e` 同时存在于 iOS 18.4 与 iOS 26.2；`iPhone 16 Pro`（18.4）与 `iPhone 17 Pro`（26.2）是不同世代。因此展示名必须带运行时。
- 物实设备 1 台：`BnTest1`（iPhone 12 / iPhone13,2 / iOS 18.3.2），`pairingState=paired` 但 `tunnelState=unavailable`、`ddiServicesAvailable=false`。**它会出现在列表里但无法安装应用**——"能被列出"与"能被启动"必须分开建模。

### 3.2 Android

- `ANDROID_HOME=/Users/kingsword09/Library/Android/sdk`
- `ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018`
- `adb` 在 PATH 上；**`emulator`、`avdmanager`、`sdkmanager` 都不在 PATH**，实体位于 `$ANDROID_HOME/emulator/emulator` 与 `$ANDROID_HOME/cmdline-tools/latest/bin/`。必须从 `ANDROID_HOME` 推导绝对路径。
- 两个 AVD：
  - `Pixel_9_Pro` — API 36 (`android-36`)、`page_size_16kb`、镜像 `system-images/android-36/google_apis_playstore_ps16k/arm64-v8a`
  - `Pixel_9a` — API 35 (`android-35`)、镜像 `system-images/android-35/google_apis_playstore/arm64-v8a`
- 已装镜像：`android-35`、`android-36`、`android-36 ps16k` 三套（均 arm64-v8a, google_apis_playstore）。
- `adb devices -l` 为空，无设备连接。

### 3.3 第一方 `android` CLI

`/usr/local/bin/android`（0.7.15232955）已安装，能力恰好覆盖 Android 侧生命周期：

```
android emulator create [--profile=<p>] [--list-profiles]
android emulator start <device> [--cold]      # 阻塞到模拟器完全就绪才返回
android emulator stop [<device>]
android emulator list [--long]
android emulator remove <device>
android sdk list|install|update|remove
android run --device=<serial> --activity=<name> --apks=<paths> ...
```

重要取舍：`android emulator create` 只能选 `--profile`（`small_phone` / `medium_phone` / `medium_tablet` / `small_desktop` / `medium_desktop` / `large_desktop`），**无法指定 API 级别、系统镜像或 ABI**。

因此本设计的分工是：

- **生命周期（start/stop/list/remove）优先走 `android` CLI** —— `start` 的"阻塞到 ready"语义正是我们需要的，省掉手写轮询。
- **需要精确版本时走 `avdmanager create avd -k <image> -d <device>`**，镜像缺失时先用 `sdkmanager` 安装。

`android` CLI 视为可选增强能力，不存在时回退到 `$ANDROID_HOME/emulator/emulator`。

---

## 4. 统一清单模型

新增 `src/device/mod.rs`：

```rust
pub enum Platform { Ios, Android }
pub enum Kind { Physical, Emulator }

pub enum State {
    /// 已就绪或已启动；Android 在线设备的 serial 放这里
    Running { serial: Option<String> },
    /// 已安装但未启动，可以启动
    Stopped,
    /// 能列出但不能用：未配对 / 隧道断开 / 镜像缺失
    Unavailable { reason: String },
}

pub struct Device {
    pub platform: Platform,
    pub kind: Kind,
    /// 稳定主键：iOS 模拟器用 UDID，Android 真机用 serial，AVD 用 AVD 名
    pub id: String,
    pub name: String,
    /// "iOS 26.2" / "android-36"
    pub runtime: Option<String>,
    /// "arm64-v8a" / "arm64"
    pub arch: Option<String>,
    pub state: State,
    /// 展示用的一行摘要
    pub detail: String,
}

impl Device {
    /// 只有为真才允许进入"可启动"候选集
    pub fn launchable(&self) -> bool {
        !matches!(self.state, State::Unavailable { .. })
    }

    /// 用于选择器与日志的展示名，必定带版本以消歧
    pub fn label(&self) -> String { /* "iPhone 16e · iOS 26.2" */ }
}
```

把 `Unavailable` 作为一等状态是这套设计的核心收益：那台 `BnTest1` 会显示为"已配对但隧道不可用（iPhone 12, iOS 18.3.2）"，在选择器里禁用并给出原因，而不是等用户选中后由 `devicectl` 抛错。

---

## 5. 发现层

原则：**全部走机器可读输出，不再做文本抓取**。

### 5.1 iOS 模拟器

```
xcrun simctl list devices available --json
xcrun simctl list runtimes --json
xcrun simctl list devicetypes --json      # 仅 create 时需要
```

`devices` 是 `{runtime_identifier: [device...]}`，每个 device 含 `udid`、`name`、`state`（`Booted`/`Shutdown`）、`deviceTypeIdentifier`、`isAvailable`。用 `runtimes` 把 runtime identifier 映射成展示名（`iOS 18.4`）。

### 5.2 iOS 真机

```
xcrun devicectl list devices --json-output <tmpfile>
```

注意 **`devicectl` 只写文件、不写 stdout**，因此固定输出到 `tempfile::NamedTempFile`（`tempfile` 已是依赖）。

取用字段：

- `identifier` → `Device.id`
- `deviceProperties.name`、`deviceProperties.osVersionNumber`
- `connectionProperties.pairingState`、`connectionProperties.tunnelState`
- `deviceProperties.ddiServicesAvailable`

判定：`pairingState != paired` 或 `tunnelState != available` 或 `ddiServicesAvailable == false` → `State::Unavailable`。

### 5.3 Android

三源合并，按 `id` 去重：

1. **AVD 元数据：直读 `~/.android/avd/<name>.avd/config.ini`** —— 零外部依赖、字段最全，且在 `android` CLI 缺失时依然可用：

   ```ini
   AvdId           = Pixel_9_Pro
   abi.type        = arm64-v8a
   hw.device.name  = pixel_9_pro
   image.sysdir.1  = system-images/android-36/google_apis_playstore_ps16k/arm64-v8a/
   tag.id          = page_size_16kb
   PlayStore.enabled = true
   ```

   从 `image.sysdir.1` 提取 API 级别与镜像路径，得到 `runtime = "android-36"`。

2. **兜底：`avdmanager list avd -c`** —— 一行一个 AVD 名，易于解析。

3. **在线设备：`adb devices -l`** —— 对每个 serial 补元数据：

   ```
   adb -s <serial> shell getprop ro.build.version.sdk
   adb -s <serial> shell getprop sys.boot_completed
   adb -s <serial> shell getprop ro.product.cpu.abi
   ```

   依据 serial 前缀判定：`emulator-*` → `Kind::Emulator`，其余 → `Kind::Physical`。

   **所有后续 adb 命令必须带 `-s <serial>`**，这是对现有 `adb_target_kind()` 的直接修正。

已装系统镜像列表（供 `create` 用候选）来自 `sdkmanager --list_installed`，或直接扫描 `$ANDROID_HOME/system-images/` 目录树。

### 5.4 路径解析

`emulator` / `avdmanager` / `sdkmanager` 不在 PATH，需从 `ANDROID_HOME` / `ANDROID_SDK_ROOT` 推导绝对路径。`doctor.rs` 已有 `android_sdk()`（`src/commands/doctor.rs:95`），提取到共用模块复用。`android` CLI 按 PATH 查找，缺失即视为无此可选能力。

---

## 6. 命令设计

### 6.1 `gpui device` 子命令组

```
gpui device list [--platform ios|android] [--all] [--json]

gpui device create --platform android --name <n>
                   [--image "system-images;android-36;google_apis_playstore;arm64-v8a"]
                   [--device pixel_9_pro]
                   [--profile small_phone]          # 快捷方式，走 android CLI

gpui device create --platform ios --name <n>
                   [--type "iPhone 17 Pro"] [--runtime 26.2]

gpui device boot <id> | --last
gpui device shutdown <id> | --all
gpui device remove <id>
```

- `--json` 直接序列化 `Vec<Device>`，供脚本与 CI 使用。
- 人类可读输出用 `colored` 渲染为按平台分组的表；默认隐藏 `Unavailable`，`--all` 才显示。

### 6.2 `gpui run` / `gpui build` 新增标志

```
--device <id>         通用，按平台解释（iOS: UDID 或 "机型@运行时"；Android: serial 或 AVD 名）
--sim <机型[@运行时]>   iOS 模拟器专用简写
--avd <name>           Android 专用
--device-only          强制使用真机（沿用现有 GPUI_IOS_DEVICE_ID 逻辑）
--allow-emulator       放行已知渲染有问题的 Android 模拟器（见 §8）
```

`build` 需要具体 UDID 才能生成正确的 `-destination`，因此 `--sim` / `--avd` / `--device` 必须在 `src/commands/build.rs` 同步接线。

---

## 7. 选择优先级与交互

统一一条解析链，从高到低：

1. **CLI 标志**：`--device` / `--sim` / `--avd`
2. **`gpui.toml` 的 `[run]` 段**（新增）：使 `gpui run ios` 可重复

   ```toml
   [run]
   ios_simulator = "iPhone 17 Pro@26.2"
   android_avd   = "Pixel_9_Pro"
   ```

3. **环境变量**：`GPUI_IOS_DEVICE`、`GPUI_IOS_DEVICE_ID`、`GPUI_ANDROID_ABIS` 全部保留，语义不变（向后兼容）
4. **交互式选择器**（`inquire::Select`）：**仅当 stdin 与 stdout 均为 TTY** 时启用；CI 与非 TTY 环境下自动跳过，避免挂起
5. **非交互自动挑选**（规则明确、可预测）：正在运行的设备 → 用户默认项 → 最新 runtime 上的首台同类机型

选择器每一项渲染为带版本与状态的整行，例如：

```
Pixel_9_Pro · android-36 · arm64-v8a · stopped
iPhone 17 Pro · iOS 26.2 · stopped
BnTest1 · iOS 18.3.2 · 不可用（已配对，隧道不可用）
```

---

## 8. 启动路径

### Android AVD（按可用性降级）

1. `android emulator start <avd>` —— 阻塞到 ready，首选
2. `$ANDROID_HOME/emulator/emulator -avd <avd> -gpu host` —— 需自行后台化
3. 以上两种都必须随后 `adb wait-for-device` + 轮询 `getprop sys.boot_completed`
4. 最后 `adb -s <serial> install -r <apk>` → `adb -s <serial> shell am start -n <bundle_id>/dev.gpui.mobile.GpuiActivity`

### iOS 模拟器

`xcrun simctl boot <udid>`（已启动会报错，忽略）→ `open -a Simulator` → `xcrun simctl bootstatus <udid> -b` → `simctl install` → `simctl launch --terminate-running-process`。

现有代码这一段是正确的，保留。

### iOS 真机

保留现有 `xcrun devicectl device install app` + `device process launch`，前置加 `launchable()` 检查。

---

## 9. Android 模拟器渲染限制（头等警告）

既有的实测结论：在 Apple Silicon 上 Android 模拟器**无法渲染 GPUI**，即使加 `-gpu host` 也不可行（无 `-gpu host` 时 wgpu 只能枚举到 `type=Cpu`；加了之后得到 `wgpu device lost: reason=Unknown` 与 `Surface texture validation error`）。需要真机，或具备原生 GPU 直通能力的宿主。

这条必须在设计里被显式处理，否则用户会在此环境浪费大量时间：

1. `gpui device list` 对 Android 模拟器打 `⚠ 可能无法渲染` 标记。
2. `gpui run android` 选中模拟器时**要求确认**（`inquire::Confirm`）；非 TTY 下必须显式传 `--allow-emulator` 才继续，文案指向使用真机。
3. 运行时二次校验：保留现有 `emulator_reports_cpu_only_gpu()`（解析 `adb shell dumpsys SurfaceFlinger` 的 GLES 渲染器，匹配 `SwiftShader` / `llvmpipe` / `ANGLE (Google, Vulkan 1.3.0 (SwiftShader`），把静态警告升级为实测确认。

---

## 10. 依赖与改动清单

**新增依赖**：`serde` + `serde_json`。

`simctl --json` 与 `devicectl --json-output` 都需要 JSON 解析，目前 CLI 侧无任何 serde 依赖。若不引入，`config.ini` 与 `avdmanager -c` 可以纯文本解析，但 simctl / devicectl 的输出不建议手写解析器。

| 文件 | 改动 |
|---|---|
| `src/device/mod.rs`、`src/device/inventory.rs` | 新增：模型、发现编排、选择链 |
| `src/device/ios.rs`、`src/device/android.rs` | 新增：平台实现 |
| `src/commands/device.rs` | 新增：`gpui device` 子命令 |
| `src/main.rs:45` | `Commands` 增加 `Device { .. }`；`Run`/`Build` 增加 §6.2 的标志 |
| `src/commands/run.rs` | 删除 `simulator_name` / `resolve_simulator` / `extract_udid` / `adb_target_kind`；改接设备层；所有 adb 命令带 `-s` |
| `src/commands/build.rs` | 接 `--sim` / `--avd` / `--device` 以生成 `-destination` |
| `src/commands/doctor.rs` | 新增 `xcrun simctl` / `xcrun devicectl` / `$ANDROID_HOME` 下的 `emulator`+`avdmanager`+`sdkmanager` / `android` CLI 检查（后者标为可选） |
| `src/template.rs` | `gpui.toml` 模板增加 `[run]` 段 |

注意：**模板是编译期通过 `include_dir!` 嵌入的**，修改 `templates/` 后必须重建 CLI，否则旧的嵌入副本仍会生效。

---

## 11. 分期实施

| 阶段 | 内容 | 风险 |
|---|---|---|
| P0 | 统一清单 + `gpui device list`（含 `--json`） | 纯只读，零风险；直接解决"如何获取已安装的模拟器与真机" |
| P1 | `run`/`build` 的 `--device`/`--sim`/`--avd` + `gpui.toml` 默认值 + 交互选择器 | 解决"在命令行选择哪个启动" |
| P2 | `gpui device boot` / `shutdown` | 开始有状态变更 |
| P3 | `gpui device create` / `remove` + 镜像选择 | 解决"是否用新模拟器" |
| P4 | `doctor` 扩展 | 低 |

---

## 附录 A：外部命令速查

| 用途 | 命令 |
|---|---|
| iOS 模拟器列表 | `xcrun simctl list devices available --json` |
| iOS 运行时列表 | `xcrun simctl list runtimes --json` |
| iOS 机型模板 | `xcrun simctl list devicetypes --json` |
| iOS 真机列表 | `xcrun devicectl list devices --json-output <file>` |
| iOS 创建模拟器 | `xcrun simctl create <name> <deviceType> [<runtime>]` |
| iOS 启动 | `xcrun simctl boot <udid>` + `bootstatus <udid> -b` |
| Android AVD 列表 | `android emulator list --long` 或 `avdmanager list avd -c` |
| Android AVD 详情 | `~/.android/avd/<name>.avd/config.ini` |
| Android 启动 | `android emulator start <avd>` |
| Android 创建 | `avdmanager create avd -n <n> -k <image> -d <device>` |
| Android 已装镜像 | `sdkmanager --list_installed` 或扫描 `$ANDROID_HOME/system-images/` |
| Android 在线设备 | `adb devices -l` + `adb -s <serial> shell getprop ...` |

## 附录 B：已知不一致

记忆中引用了仓库内的 `docs/TROUBLESHOOTING.md` 作为 §9 渲染问题的出处，但该文件既不在已跟踪文件列表中，`docs/` 目录此前也不存在。需补上该文档，或把引用改到实际位置。