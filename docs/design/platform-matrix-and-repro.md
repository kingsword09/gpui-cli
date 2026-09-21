# 设计契约：平台 runner、设备租约、矩阵与复现包

状态：拟议，未实现。任务：M01–M06、Q01。依赖：[场景](scenarios-and-checks.md)、[观察协议](observation-protocol.md)。

## 1. 平台能力必须独立验收

| 目标 | 第一实现方式 | 必须说明的边界 |
| --- | --- | --- |
| macOS | 应用 runtime + scene/window capture | 首先验证 GPUI readback；平台截图权限单独判定 |
| iOS simulator | simctl 安装/启动/设备截图 + runtime | 设备截图包含系统 UI；启动确认与 UI 心跳分离 |
| Android emulator/device | adb 安装/启动/screencap/logcat + runtime | 按 serial 和应用身份过滤；ABI 必须匹配设备 |
| Windows | 原生 runtime 与窗口截图 adapter | 跨编译不是运行验证；窗口/DPI/输入要在 Windows runner 测试 |
| Linux | 原生 runtime，明确 Wayland/X11/backend | 图形会话和截图权限不同；无显示器能力另做 PoC |
| iOS physical | 独立实验 | 通道连接、日志、签名与捕获不能直接沿用模拟器结论 |

M01 首批交付 Android emulator 与 iOS simulator；Windows/Linux 的真实 UI 在相应 runner 准备好后独立晋升。桌面预览帮助验证共享逻辑，但不替代移动键盘、权限、生命周期和原生嵌入视图。

## 2. Runner 接口和证据

拟议接口仅作实现边界，不是现有 Rust API：

```rust,ignore
trait Runner {
    fn describe(&self) -> RunnerInfo;
    fn capabilities(&self, target: &Target) -> CapabilitySet;
    fn prepare(&self, plan: &RunPlan, lease: &Lease) -> Result<PreparedRun>;
    fn launch(&self, run: &PreparedRun) -> Result<LaunchEvidence>;
    fn capture(&self, scope: &CaptureScope) -> Result<Artifact>;
    fn collect_logs(&self, scope: &RunScope) -> Result<LogStream>;
    fn stop_owned(&self, run: &RunScope) -> Result<StopEvidence>;
}
```

`RunnerInfo` 包含 runner_id、host_id、OS/版本/arch、CLI/runtime/协议版本、SDK工具版本、设备目录、可用资源和认证方式。每次请求必须验证 target/run/lease，不能通过“最近连接设备”推断目标。

安装、启动、通道连接和进程探测各自产生证据事件。移动应用断线仅改变 channel 状态；只有平台退出/崩溃证据才能更新 process 为 exited。无法归属到某次 run 的原生日志保存为 unassigned，不塞给最新 run。

### 2.1 截图策略

- iOS simulator：`simctl io <udid> screenshot`，记录设备方向、屏幕尺寸、当前前台应用证据。
- Android：`adb -s <serial> exec-out screencap -p`，二进制读取；禁止把截图经文本转码。
- desktop：优先已验证的 scene capture；OS window capture 记录窗口 id 和权限。
- 捕获前后核对 run、前台应用和 scene，最多重试一次。系统弹窗可作为场景的 OS 层证据，但不能标成 app scene。
- screenshot provider 不支持某种遮挡/后台捕获时返回 unavailable；不能偷偷截取整个桌面。

### 2.2 原生日志

Android logcat collector 在每次启动后更新包/PID过滤并记录日志来源，保存 native crash、ANR 和早期输出。PID 重用时结合启动时间/平台进程身份核验。iOS simulator 使用可用的 simctl/系统日志路径并保存关联置信度。两者必须有停止、排空和容量上限。

Maestro 等外部引擎可以用作 OS 输入适配器，但先验证 GPUI 自绘控件是否进入平台无障碍树；没有树时不能承诺稳定 ID 定位。场景格式与结果模型由 gpui-cli 持有，外部工具只是一个 backend。

## 3. 设备与窗口租约

### 3.1 为什么需要独占

多个 worktree、Agent 或 CI 任务可能同时选择同一模拟器。仅用不同 session 目录不能阻止互相安装、抢占前台和改写设备侧配置。凡是安装、启动、输入、reset、profile/capture workload，必须持有对应设备/运行资源的写租约。

只读发现和不改变设备状态的日志查询可并行。桌面独立应用运行可以并发，但同一窗口的场景动作必须串行。

### 3.2 本地租约协议

租约按 `(host_id, platform, stable_device_id)` 建立，保存于主机级 gpui runtime/cache 目录，不能只放在应用项目内。目录位置按 OS 约定解析，不扫描或删除其他工具目录。

实现使用 OS 排他文件锁和持有锁的进程句柄；metadata 包含 lease_id、owner_session、project_id、PID/启动身份、创建时间、heartbeat、递增 fencing_token。

- heartbeat 默认 10 s；30 s 未更新标 suspect。TTL 不是自动抢锁授权。
- OS 锁仍有效时其他任务返回 device_busy，附 owner 的非敏感信息和等待方式。
- 只有锁已释放且 owner 身份确认失效时才能恢复陈旧 metadata。
- 每次新租约递增 fencing_token；runner 拒绝旧 token 的迟到操作。
- 设备断开时租约进入 lost，停止后续输入；同 serial 重连后重新探测并申请新租约。
- supervisor 崩溃后，不通过全局进程名匹配来清理应用；只处理持有记录、身份仍匹配的资源。
- 不自动关闭用户启动的模拟器。只有本任务创建的临时设备才在配置允许时清理。

多窗口操作增加 `(run_id, window_id)` 子租约。多个 Agent 可以同时观察，但一个窗口只能有一个动作序列 owner。人工输入污染判定见场景设计。

## 4. 矩阵配置

完整示例见 [matrix.toml](../examples/matrix.toml)。字段：

| 字段 | 默认/要求 |
| --- | --- |
| `schema_version` | 1 |
| `source_mode` | frozen；首次正式矩阵只支持 frozen |
| `max_parallel` | 2，必须大于 0，并受 runner 资源限制 |
| `fail_fast` | false；失败目标不阻止独立目标收集证据 |
| `targets[].id` | 矩阵内唯一 |
| `runner` | local 或预先配置的远程 runner_id |
| `platform` | macos/windows/linux/ios/android |
| `device` | 移动目标必须显式提供稳定 ID/可解析 spec |
| `abi` | Android 必需；调度前与设备能力核对 |
| `required` | true；缺失目标不能使整个矩阵通过 |
| `scenarios` | 显式 ID 列表 |
| `timeout_ms` | 目标总 deadline，包含租约等待和启动 |

本地 macOS runner 不能直接运行 Windows cell。配置远程 runner 之前，Windows 目标应报告 runner_unavailable，不能只做交叉编译后标通过。

### 4.1 结果聚合

每个 `(target, scenario)` cell 为 passed / failed / inconclusive / unavailable / cancelled。required cell 全部 passed 才有资格通过。聚合顺序：任一 required failed → failed；否则存在 required 非 passed → inconclusive（整体用户取消时为 cancelled）；否则有任一 optional 非 passed → partial；所有 cell 均 passed → passed。optional 失败也必须展示，不能计入已通过平台数。

failed 表示已知断言不满足；inconclusive 表示结果缺少足够证据。汇总必须保留两者区别。JSON 包含所有 cell、源码快照哈希、场景哈希、运行环境、产物引用和未执行原因。

MVP 单个目标内场景串行；跨目标并行。不同场景共享构建产物，但不能共享未 reset 的应用状态。

## 5. 冻结输入与构建共享

### 5.1 快照内容

快照在调度前一次性建立，包含源码、Cargo.lock、场景/fixture、assets、原生宿主配置、工具链描述和显式收集的外部 path dependency。使用 `cargo metadata` 找到依赖边界；缺失的外部输入必须报错或明确标记不可复现。

不在快照内的内容：`.git` 凭据、构建输出、Live token、开发者的整个 home、未白名单化环境变量。原生签名秘密通过 runner 配置注入，manifest 仅保存键名和来源标识。

普通开发目录保持可编辑。快照构建在独立目录中运行；同一 matrix 的所有 cell 使用同一 content hash。外部链接目录不能被隐式递归拷贝，必须先纳入允许的依赖根并检查循环/路径逃逸。

MVP 使用内容寻址文件与冻结 manifest：采集后重核源输入、校验复制内容、再原子发布不可变目录；最多重试两次，持续变更返回 unstable_inputs。frozen_snapshot 保证所有消费者使用相同字节集合，不声称可在任意普通文件系统取得某一墙钟时刻的原子全盘快照。build.rs/插件任意读取未声明文件或网络时，记录 untracked_inputs 并拒绝“完全可复现”的声明；严格任务要求补声明或隔离这些输入。

### 5.2 构建键

`BuildKey = source_manifest_hash + Cargo.lock_hash + target_triple + profile + features + native_config_hash + toolchain_fingerprint + relevant_env_hash + preview_registry_hash`。

哈希的环境白名单由工具链设计定义。不能只用源码 mtime、crate 名或 Git commit 作为缓存键；未提交改动同样必须参与。

同主机相同 BuildKey 的在途构建可合并。调用方取消只移除自己的引用；没有订阅者时才考虑终止构建。发布缓存前验证完整产物 manifest；失败或部分结果不可作为命中项。

Android 的 JNI 库必须按 ABI、profile 和 BuildKey 隔离，不能让 release/多 ABI 作业同时覆盖同一 `jniLibs` 目录。iOS DerivedData 也按 BuildKey/目标隔离。

## 6. 视觉差异和平台契约

先比较共同的行为断言，再处理各平台的视觉基线。

视觉 BaselineKey 至少包含：scenario、fixture、platform、backend、OS主要版本、viewport实际尺寸、DPI、theme、locale、字体集合摘要和捕获 scope。

- 基线不存在返回 baseline_missing；不自动将当前截图接受为基线。
- 尺寸/DPI/方向/scope 不一致时先判不可比，不把缩放后的相似图当作通过。
- 视觉算法 v1 使用像素差与明确容差，输出 diff 图和区域；容差、mask 和算法版本进入 manifest。
- 动态区域 mask 来自受版本控制的场景配置，不能由 Agent 在失败后自动扩大。
- 基线更新是独立 review 操作，PR 展示旧/新/diff 和原因；修复流程不能用更新基线自动消除失败。
- 跨平台布局可比较“不裁剪”“按钮可见”“安全区内”等约束，不要求系统字体逐像素一致。

## 7. 复现包格式与工作流

拟议命令：

```bash
gpui repro export --check check-42 --output ./login-failure.gpui-repro
gpui repro inspect ./login-failure.gpui-repro --json
gpui repro run ./login-failure.gpui-repro --target android --json
```

`inspect` 只校验和读取，不运行包内代码。`run` 是明确的执行操作：展示源码来源、工具链和运行目标；不自动下载未知二进制或执行包内 shell 脚本。

包是带 manifest 的 ZIP；完整草案见 [repro-manifest.json](../examples/repro-manifest.json)。

```text
manifest.json
inputs/source-manifest.json
inputs/scenarios.toml
inputs/fixtures/...
events/events.ndjson
results/check.json
artifacts/screenshots/...
artifacts/trees/...
artifacts/diffs/...
logs/...
```

manifest 包括 schema_version、producer/协议版本、base commit、工作区差异引用、source hash、环境要求、每个文件的路径/大小/哈希、失败步骤和隐私筛选结果。

files 索引覆盖全部 payload，排除 manifest.json 自身，避免自引用 hash；包摘要在包外/上层 artifact metadata 记录。导出裁减场景集合时同步改写场景文件及其 hash，使其中 fixture 引用都能在包内解析；保留 original hash 与导出映射，不伪称裁减后的字节等于原件。

默认导出诊断与产物，以及源码定位信息。源码/patch 是显式 `--include-source` 或 `--include-patch` 选项；不包含源码时标记 `replayability: requires_source`。包含代码仍可能缺少私有依赖/秘密，必须列出 prerequisites，不承诺离线可重放。

### 7.1 筛选与完整性

- 不导出 session/app token、签名密钥、完整环境变量和未筛选的用户输入。
- 场景可以声明敏感字段与截图 mask；导出前列出包含的文件类别和筛选结果。
- 筛选后的产物重新计算哈希，并标记 redacted；不能与未筛选文件共用同一 artifact_id。
- 导入检查路径 traversal、绝对路径、symlink、重复路径、压缩炸弹和不符哈希。
- 默认解压总量上限 512 MiB、10,000 文件；额外 GPU 捕获走显式扩展配额，不自动提高上限。
- 未验证导入对象不得加入正常 artifact 索引。报告的 HTML 内容必须转义，不能执行包内提供的页面脚本。

## 8. 远程 runner M06

远程是本地矩阵稳定后的扩展，不是首个观察闭环的依赖。

第一版采用显式登记的 runner 和短期任务进程，可由 SSH 启动 `gpui runner serve --stdio`。不引入永久云服务。SSH 主机和身份由用户配置，工具不自动探测或连接任意网络地址。

协议步骤：认证通道 → 握手/能力/工具链 → source manifest 对账 → 缺失内容上传 → 申请租约 → 执行 → 拉取结果 → 释放。remote 的本地路径不能作为客户端路径直接使用。

请求具备 job_id、request_id 和 fencing_token。网络重试先查 job 状态，不重复安装和点击。连接丢失后有界保留任务；默认继续已执行的只读捕获/本地检查，禁止接受新的输入。到期清理本任务拥有的进程和租约，恢复客户端收到 terminal 或 outcome_unknown。

离线/配额/架构不符在分发前失败；不退到另一台未声明设备。需要 Apple 平台的目标必须放到具备相应工具链和设备权限的 runner。

## 9. CI 分层与清理

| 层 | 执行内容 | 环境要求 |
| --- | --- | --- |
| L0 | 解析、协议、结果聚合、租约竞争、归档安全 | 普通 CI，无 GPU |
| L1 | 生成模板 cargo check、Android 宿主打包 | 当前 CI 的增强版 |
| L2 | 实际 GPUI 构建、真实窗口或模拟器场景 | 有图形会话/GPU/SDK 的 runner |
| L3 | 多平台视觉/性能、真机、外部 GPU 捕获 | 固定硬件/自托管或明确配置的设备池 |

失败上传 summary、check结果、截图/树/diff、限额日志和可导出复现包。大 GPU trace 按独立保留策略处理。清理按 owned resource 清单执行；退出、取消、超时、supervisor 崩溃均需要测试。

## 10. 验收

M01 对应 M-01/M-02；M02/M03 对应 M-03–M-06；M04 对应 M-07/M-08；M05 对应 R-01–R-05；M06 对应 M-09/M-10。详细输入、动作和预期见 [验收矩阵](../roadmap/acceptance-matrix.md)。
