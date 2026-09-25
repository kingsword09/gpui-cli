# 设计契约：版本绑定的 UI 观察协议

状态：O01 窗口注册/UI heartbeat、O02 资源 ACK、O03 有界产物库、scene_completed
事件、supervisor build request、有界 operation 状态机和 observe 提交已实现；
scene readback、完整语义导出与成功观察编排仍拟议；O05/O06 已提供受控语义树产物
和只读查询第一切片。基线：`1550bf3`。
任务：F02、O01–O06、A01。
上位文档：[总路线](../ROADMAP-agent-native-development.md)。

## 1. 范围与不能妥协的约束

本协议服务于窗口发现、UI 响应检测、资源确认、截图/树查询以及后续场景操作。第一交付目标为 macOS，移动端按能力适配。

必须满足：

1. 一条结果只能描述明确的项目、session、build、run、window 和内容版本。
2. 编译成功、启动成功、通道连接、UI 响应、scene 完成、GPU 呈现、断言通过是独立事实。
3. 查询和等待不能占用 UI 线程或事件存储锁来等待 I/O。
4. 输入变化后旧结果仍可供查看，但不能作为当前版本验证成功的证据。
5. 截图、树、日志的大对象使用产物引用；1 MiB 帧上限保持不变。
6. 无能力、无权限、超时、断线、内容变化都返回明确状态；不能伪造空树或复用旧截图并换上新版本号。

## 2. 版本与兼容策略

### 2.1 四个独立版本维度

| 字段 | 用途 | 演进规则 |
| --- | --- | --- |
| `schema_version` | 控制 API 与外部 JSON | D1 为 1；新功能用 2，旧命令默认仍请求 1 |
| `proto` | CLI ↔ app 握手/消息 | 旧模板为 1；新 runtime 为 2，双解码 |
| `runtime_version` | 应用开发支持库版本 | semver；握手中声明，不等同于 GPUI 版本 |
| `template_version` | 生成项目文件集版本 | 用于升级和能力来源追踪，不作为协议协商值 |

新 CLI 给 `dev` 增加拟议参数 `--schema-version 1|2`。D1 的 status/diagnostics/events 默认继续输出 v1。observe/query/act/operation/artifact 只支持 v2；未指定时这些新命令自动选择 v2，显式要求 v1 则返回 `unsupported_version`。

新注册文件保持旧的 `schema_version: 1` 和原有字段，添加 `supported_schema_versions: [1,2]`。旧客户端忽略新字段，继续请求 v1。新客户端连接旧 supervisor 时，D1 可用，新操作返回 `upgrade_required`，不能未经协商发送新命令。

### 2.2 兼容不只包括握手

当前旧客户端将事件 `kind` 解码为封闭枚举。新事件不能直接混入它的 v1 事件页或退出恢复日志。

- v2 是内部规范事件模型。v1 adapter 保留 D1 已知事件的字段和布尔能力；v2 专属事件一对一投影为兼容的 `app.log`，`data.level=debug`、`data.source=protocol_v1_projection`，正文只说明被省略的事件 kind，不包含新 payload。
- 两种表示使用同一 seq。禁止直接丢弃 v2 专属事件：已发布旧 CLI 的离线 reader 会把过滤造成的序号空洞当成丢失。兼容占位日志不得改变健康状态或制造 runtime error。
- v1 页的 `next_seq` 是最后一条投影事件的 seq；空页保持原游标。真实保留窗口外的请求仍报告 gap / `cursor_expired`，不能用占位补齐已淘汰的事件。
- 根目录 `events-*.ndjson` 保持 v1 可读投影，供已发布旧 CLI 在 supervisor 退出后恢复；v2 完整日志放入 `journal-v2/`。
- 两份日志分别有配额，分别报告实际最早保留 seq。新 CLI 的 archive reader 依据 manifest 选择日志，不能混读后产生重复 seq。根目录 `state.json` 继续是 v1 投影；v2 快照放入 `journal-v2/state.json`。
- v1 `stale` 计算语义保持原样；v2 新增更细的 `freshness`，不偷偷改变 v1 布尔值。
- 原有 app v1 的 logs/panic/state/assets 继续工作；它不能满足帧确认、语义和输入能力，返回 `upgrade_required`。

F02 必须保存一个基线 CLI 二进制/测试客户端及 v1 模板夹具，实际执行上述兼容测试。

## 3. 身份与数据模型

### 3.1 身份表

| 名称 | 创建者 | 生命周期 |
| --- | --- | --- |
| `project_id` | supervisor | 规范化项目路径标识；导出时使用不含绝对路径的替代标识 |
| `session_id` | supervisor | 一次 Live 生命周期 |
| `target_id` | runner | 平台、设备和宿主标识；不是人类显示名称 |
| `build_id` | supervisor | 一次构建尝试，包括失败和 superseded |
| `run_id` | supervisor | 一次进程启动，不能用 PID 替代 |
| `window_id` | runtime | 当前 run 中一个窗口，从创建到关闭 |
| `scene_epoch` | runtime | 当前窗口已完成 scene 的递增编号 |
| `presented_frame_id` | backend | 已验证呈现的帧；不能提供时为 null |
| `request_id` | 客户端 | 一次逻辑请求；重试操作必须复用 |
| `operation_id` | supervisor | 异步操作及其终态 |
| `observation_id` | supervisor | 不可变的观察结果 |
| `artifact_id` | 产物层 | 不可变对象引用，绑定内容 SHA-256 |

作用域比较至少使用 `(session_id, run_id, window_id)`；所有编号都不能仅凭字符串在不同会话之间比较。

客户端 request_id 限1–128个ASCII字母/数字/点/横线/下划线，禁止把任意大文本作为去重键。参数哈希基于去除传输重试字段后的规范化参数，包含目标/动作/内容；hash算法和规范化版本固定在协议类型中。

### 3.2 内容版本

一个 `Revision` 包含 `source_revision`、`asset_revision`。这些计数仅在 session 内有意义；跨 runner 比较必须使用输入清单的 `input_hash`。

新增内容：

- `input_consistency: tracked_scan | frozen_snapshot`。
- `source_hash` 和 `asset_hash`：按规范化路径和文件内容排序计算的 SHA-256。
- 预览热参数实验另有 `overlay_revision`，默认 0；非零结果必须标记 `preview_only`。
- `freshness.source`：`current | stale | unknown`。
- `freshness.assets`：`applied | sent | stale | unknown`。
- `freshness.scene`：`matches | stale | unknown`。

完整版本匹配要求源码、资源、overlay 与 observation 锁定的目标相同。普通 Live 的输入扫描不是原子文件系统快照，必须标注 `tracked_scan`；严格场景/矩阵使用冻结输入。

### 3.3 独立状态

| 维度 | 状态 |
| --- | --- |
| build | idle / queued / building / succeeded / failed / superseded / cancelled |
| process | absent / starting / launched / running / exited / unknown |
| channel | disconnected / connecting / connected |
| UI | unknown / responsive / unresponsive / suspended / unavailable |
| capture | pending / captured / unavailable / failed |
| check | not_run / running / passed / failed / inconclusive |

移动端只有启动命令成功时保留 `launched`；失去通道后不凭此断言 process exited。长时间静态界面不意味着 UI 卡死。

## 4. v2 能力声明

每项能力必须包含 `available`、`reason`、`provider` 和 `constraints`。没有支持时 `reason` 为稳定代码及可读说明。

```json
{
  "ui.heartbeat": {
    "available": true,
    "reason": null,
    "provider": "gpui-dev-runtime",
    "constraints": {"foreground_only": true}
  },
  "capture.scene": {
    "available": false,
    "reason": "backend_unsupported",
    "provider": "platform-adapter",
    "constraints": {}
  },
  "capture.window": {
    "available": true,
    "reason": null,
    "provider": "macos_screencapture",
    "constraints": {"consistency": "best_effort", "scope": "window"}
  },
  "capture.device": {
    "available": true,
    "reason": null,
    "provider": "simctl",
    "constraints": {"consistency": "best_effort", "scope": "device_screen"}
  }
}
```

规范能力名：`ui.heartbeat`、`windows.list`、`capture.scene`、`capture.window`、`capture.device`、`semantics.read`、`semantics.bounds`、`input.pointer`、`input.keyboard`、`assets.applied`、`scenario.reset`、`metrics.cpu`、`metrics.gpu`。

能力按 app、backend、设备权限的交集计算，不只按操作系统名称预设。运行中权限撤销或窗口关闭会生成 `capabilities.changed`；旧缓存不能继续使用。

## 5. 应用协议 v2

### 5.1 握手与认证

保留长度前缀 TCP 和每次启动轮换 token。新 hello 增加 `runtime_version`、`gpui_version`、`capabilities`；build/run 身份由 supervisor 的预期 token 绑定，不能信任客户端自行声称的身份。

新 supervisor 同时接受 proto 1/2。旧 supervisor 拒绝 proto 2 时，runtime 可以新建连接回退到 v1，但必须关闭全部 v2 能力；认证失败不触发降级重试。控制 token 与应用 token 不互用，产物读取也验证会话身份。

### 5.2 新消息表

| 消息 | 方向 | 关键字段 |
| --- | --- | --- |
| `window_registered` / `window_closed` | app → CLI | window_id、标签、尺寸、DPI、生命周期 |
| `probe_ui` / `ui_probe_result` | 双向 | request_id、window_id、UI 排队/执行时间、前后台状态 |
| `assets_begin` / `assets_commit` | CLI → app | transfer_id、目标 asset_revision、路径/哈希/删除项 |
| `assets_applied` | app → CLI | asset_revision、成功/失败路径、缓存失效结果、必要资源状态 |
| `capture` / `capture_result` | 双向 | request_id、window_id、目标版本、scene_epoch、产物 transfer_id |
| `scene_completed` | app → CLI | window_id、scene_epoch、使用的 revision、可选 presented_frame_id |
| `semantics_query` / `semantics_result` | 双向 | observation_id、节点引用、投影、分页游标 |
| `artifact_begin/chunk/end` | 双向 | transfer_id、声明大小、SHA-256、offset、内容块 |
| `cancel_request` / `request_finished` | 双向 | request_id、终态、是否已经执行 |

输入与场景消息在 [场景设计](scenarios-and-checks.md) 定义，复用同一 envelope。所有有回复的消息必须关联 request_id，不能靠“等待下一个类型匹配的消息”路由。

### 5.3 UI 线程边界

网络线程负责鉴权、解码、限流和请求入队。只有 runtime 的 UI adapter 能访问窗口、树和正常输入分发；它把短任务投递到 UI 线程，完成后回传不可变结果。

UI 线程禁止等待网络、写大文件、编码 PNG、读取整个源目录或获取长期持有的会话锁。scene 读回可能需要 backend fence，必须由异步状态机完成。队列满时返回 `busy`，不能无限堆积。

## 6. 操作模型与 CLI 语义

### 6.1 异步核心

操作状态：`queued → running → succeeded | failed | cancelled | timed_out | superseded | unknown`。终态不可被迟到回复覆盖。

- 提交立即返回 operation_id；普通 CLI 等待该操作，`--async` 则直接交还 ID。
- CLI 等待使用 `operation.get` 的有界长轮询，不让控制 handler 持锁等待构建/截图。
- observe/act 等单次叶操作默认总 deadline 30 s、最长 120 s；单次长轮询最多 30 s。check/matrix/perf 是由有界子操作组成的 job，总 deadline 由计划明确给出，不受单次观察的 120 s 限制；提交/查询仍有界，不能把一个连接悬挂数小时。
- CLI 断开不会把已接受操作重新执行。显式 cancel 才改变操作状态；读操作取消可及时终止，已投递输入取消可能返回 unknown。
- 一个 run 最多同时有 4 个观察操作；同窗口同版本同选项的观察可共享采集，但每个请求保留独立 deadline 和结果引用。

终态查询记录单session最多2048项或8MiB，结束10min后可淘汰；活动任务不得回收。淘汰后 operation.get 返回 operation_expired，不复用ID。动作的持久去重墓碑独立于查询缓存，缓存过期不授权重新投递，见场景设计。新提交也受有限队列约束，无法登记就返回 busy/quota_exceeded。

### 6.2 拟议命令

```bash
# query 需要成功 observation；其余接口仍按计划逐步实现。
gpui dev windows --schema-version 2 --json
gpui dev observe --sync --window w1 --require screenshot --timeout 30s --json
gpui dev observe --sync --window w1 --require screenshot,semantics --async --json
gpui dev operation get op-7 --wait 30s --json
gpui dev operation cancel op-7 --json
gpui dev query --observation o-7 --id counter.increment --json
gpui dev artifact get a-7 --output ./observation.png
```

`observe` 默认要求 UI 响应和 screenshot；semantics 是可选产物，要求它时必须显式列入 `--require`。无窗口选择且有多个候选时返回 `ambiguous_window`。没有 `--sync` 时仅观察所选运行版本，不触发构建，结果如实标 stale。

`--require` 和场景 `requires` 使用同一解析规则：`screenshot` 是 `capture.scene | capture.window | capture.device` 中满足请求 scope 的一个可用 provider，默认按该顺序选择并回报实际 provider；`semantics` 对应 `semantics.read`；其余项必须是规范能力名。严格要求 scene 时不能退化到 device_screen。成功观察的 `ui.heartbeat` 始终为隐含必需项。

`--sync` 会请求构建/重启及资源同步，不能标为纯只读工具。默认客户端选择规则继续要求多 session 显式指定 `--session`。

### 6.3 成功与退出码

v2 外层保留 `schema_version/session_id/ok/result|error`，增加 request_id。完整成功例子见 [observation-v2.json](../examples/observation-v2.json)。

- `ok` 描述本次 API 调用是否成功。异步 submit 的 `ok:true` 不代表操作已通过。
- `operation.get` 查询成功可以返回 `ok:true` 和 `state:failed`；调用者必须看终态。
- 等待型 observe/check 失败时 CLI 返回 1，`ok:false`，错误 details 可带 operation_id 和部分产物。
- 无效 CLI 语法退出 2；v2 `--json` 模式在可识别该模式时输出统一参数错误 envelope，stderr 可补充帮助。
- 用户取消退出 130；机器结果保留 `cancelled`。stdout 只放 JSON/NDJSON；进度文字走 stderr。
- 可选产物缺失但所有 require 条件满足时可以成功，`completeness:partial` 必须指出原因。

## 7. `observe --sync` 算法

1. 解析 project/session/window，读取能力；缺少必需能力立即返回 `unavailable` 或 `upgrade_required`。macOS `screencapture` provider 只能提供 `best_effort/window`，不能替代 `capture.scene`。
2. 在输入扫描锁内做主动内容扫描，生成目标 revision 和 input_hash；记录 `tracked_scan`。不能用当前 watcher 计数代替扫描。
3. 登记操作、目标 revision、截止时间，再向构建编排器提交“确保该版本”的请求。若已有对应构建则等待；运行版本已匹配则不重建。
4. 编排器在构建前后重核输入。输入变化则将原操作标为 superseded，新的 Live 构建可以继续，但原操作不偷偷改目标。
5. 编译失败立即结束原操作并返回相关诊断，旧 app 的产物只能作为 stale 参考。
6. 选择对应 build 产生的 run，等待连接、所选窗口和 UI 探测。每一步检查 run 是否已被替换。
7. 确认该次资源事务完成。所有场景声明的必需资源必须已加载成功；非必需资源可以记录 not_requested。
8. 请求相关窗口完成一次使用目标版本的 scene，等待需要的静稳条件，再采集截图及可用语义树。
9. 原子读取 UI 数据或校验采集前后 scene/revision。采集中变化则重试一次；再次变化返回 `capture_unstable`，不无限延长 deadline。
10. 再做输入核验和运行身份核验，生成不可变 observation，分配 observation_id，提交产物和事件。

必须把第 3 步接入构建编排器；当前 `run_cycles` 只处理 watcher/键盘事件，单纯在 control handler 中扫描不等于能触发构建。

跟踪扫描无法证明外部依赖、环境变量或文件系统竞态完全固定，结果必须带 `input_scope` 和 `untracked_inputs`。严格检查必须采用冻结快照流程，见 [平台矩阵设计](platform-matrix-and-repro.md)。

## 8. UI 心跳、帧与静稳

### 8.1 UI 响应检测

前台窗口默认每 1 s 投递一次轻量 probe；一个窗口最多一个在途 probe。等待型 observe 额外发起关联 request_id 的即时 probe。

3 s 未执行 probe 可标记 `unresponsive`，但只有在 supervisor 和 app 未休眠、应用前台状态已知时生效。系统休眠/窗口挂起期间为 suspended；状态未知则 unknown。恢复后重置截止时间，不能把休眠计为 UI 卡死。

该阈值是诊断策略，可配置；它不是声明某次业务请求必须 3 s 完成。

### 8.2 完成帧定义

`scene_epoch` 只在 GPUI 完成构建可用于读回的 scene 后增加。`on_next_frame` 回调不是完成屏幕呈现的证明。真正的 `presented_frame_id` 仅由已验证的 backend 完成通知提供，没有则 null。

PoC 必须记录：截图读取哪个 scene、文字/布局数据什么时候冻结、GPU readback fence 的生命周期、窗口关闭和设备丢失时的回调行为。不能通过 sleep 一个固定时长来推断呈现。

### 8.3 静稳条件

默认 settle 窗口 300 ms：目标源码/资源匹配、UI 可响应、必需资源就绪、相关 scene/布局无变更。连续动画可以返回 `settled:false` 和 `continuous_animation`；观察本身可在允许非静稳的请求中完成，严格视觉断言必须有固定动画时刻或显式场景 ready 条件。

## 9. 截图与语义一致性

| 等级 | 条件 | 允许用途 |
| --- | --- | --- |
| `same_scene` | 图像和树来自同一个已完成 scene_epoch | 场景级联合定位；不等同于屏幕已呈现 |
| `best_effort` | 平台截图前后身份/scene 不变，时间范围明确 | 原生宿主/设备截图；暴露其采集范围 |
| `partial` | 某种产物缺失或一致性无法建立 | 排查参考，不满足要求该产物的断言 |

截图元数据必须包含 capture provider、scope（scene/window/device_screen）、像素尺寸、逻辑尺寸、scale、方向、是否含系统栏、采集起止时间、前后 scene_epoch。设备截图还需要前台应用确认；无法确认则不能接受为目标应用的验证截图。

语义节点规范字段：`node_ref`、可选 `logical_id`、role、name、value、enabled、focused、bounds、clip_bounds、children、可选 source_location。不可得的字段为 null 并标记 unsupported，不能填写猜测值。

- `node_ref` 仅在 observation 中有效；下一次观察重新定位。
- `logical_id` 来自开发者声明的 accessibility ID，重复列表项结合稳定业务 key；不使用导出中的临时 a/b/c 别名。
- 查询支持按 logical_id、role/name、父节点及分页。默认 200 节点或 128 KiB，先到者截断并返回游标。
- 变化摘要显式接收 before/after 两个 observation。唯一 `logical_id` 的节点按 added/removed/changed/unchanged 比较；同一 logical ID 的临时 `node_ref` 变化不算节点替换。
- 缺失或重复 `logical_id` 的节点不与另一棵树猜测配对，报告为 `subtree_replaced`，并保留两侧 observation、artifact、run 身份。跨 run 的显式比较允许执行，但 `same_run=false`。
- diff 结果最多返回 200 条记录和 128 KiB；完整计数保留在 `summary`，被边界截断的类别进入 `omitted`。
- `.id()` 不自动视作 accessibility ID。自绘 div 缺语义时返回明确缺口。
- source_location 是可选 debug 信息；生成代码/宏没有准确映射时返回 unknown。

## 10. 资源事务与产物传输

### 10.1 资源应用

一组变化使用 transfer_id 和目标 asset_revision。app 验证哈希、暂存文件，再在 UI 线程统一使相关缓存失效；成功/失败路径都需要 ACK。删除必须是协议内显式操作，不以宿主 `read` 失败代替删除消息。

ACK 分开描述 `received`、`cache_invalidated`、`required_loaded`。只有必要资源已加载且后续 scene 使用该 revision，才能判定观察资源匹配。恢复连接时按 manifest 对账，不假设上次发送全部到达。

### 10.2 产物生命周期

`declared → receiving → verified → published → expired`。哈希和大小校验前不可分配给成功观察；文件写入使用临时文件加原子提交。接收中断清理未发布临时文件。外部请求只使用 artifact_id，不接受任意宿主路径。

第一阶段：桌面 runtime 写入被授予的单一会话临时目录或走分块通道；移动端截图优先由 runner 取得。两种路径都经相同产物注册与配额校验。

| 资源 | v2 初始上限/策略 |
| --- | --- |
| 单帧 | 1 MiB，沿用当前上限 |
| 单块原始二进制 | 128 KiB，base64 后仍留足 envelope 空间 |
| 同时传输 | 每 run 2 个；超出返回 busy |
| PNG | 16 MiB 且像素总数 ≤ 20 MP；先检查尺寸防解码膨胀 |
| 全树产物 | 16 MiB / 50,000 节点，查询必须分页 |
| UI 命令队列 | 每 run 64 条；probe 合并，用户输入不静默丢弃 |
| 操作终态缓存 | 每 session 2,048 项 / 8 MiB，终态保留10 min；活动操作不淘汰 |
| 事件页 | 128 条 / 512 KiB |
| 内存事件 | 2,048 条 / 4 MiB 主日志；投影避免复制完整负载 |
| 磁盘事件 | v1 与 v2 各 8 MiB；原始输出继续 32 MiB |
| 普通产物 | 每 session 256 MiB；项目总量 2 GiB；结束 7 天可清理 |
| GPU trace | 独立配额，见性能设计；不通过普通截图接口发送 |

清理不能删除活动操作、打开的报告或显式 pin 的产物。不能回收足够空间时返回 `quota_exceeded`。引用失效返回 `artifact_expired`，不能指向后来复用的文件。

## 11. 错误契约

| code | 触发 | 自动重试策略 |
| --- | --- | --- |
| invalid_project / invalid_request | 参数或项目格式错误 | 修正请求 |
| unauthorized / permission_denied | 身份或能力授权缺失 | 不自动改配置 |
| unsupported_version / upgrade_required | 无共同版本或 runtime 太旧 | 继续 D1；提示升级 |
| ambiguous_session / ambiguous_window | 多个目标且未明确选择 | 要求调用方选择 |
| unavailable | backend/权限/平台不支持必需能力 | 返回具体 capability，不伪造 |
| build_failed / launch_failed | 对应阶段失败 | 新编辑或明确重试 |
| superseded | 目标输入在等待中变化 | 调用方重新建立观察目标 |
| stale_observation | 操作使用旧 run/内容/节点 | 重新 observe/query |
| ui_unresponsive / capture_unstable | UI 未响应或采集变化 | 有界重试，保留证据 |
| target_exited / channel_lost | 运行或通道证据 | 分别记录，不互相替代 |
| busy / quota_exceeded | 并发或存储上限 | 退避/清理后再试 |
| artifact_expired / checksum_mismatch | 引用过期或传输损坏 | 重取产物，不能继续使用旧引用 |
| operation_expired | 操作查询记录已淘汰 | 读取已保存证据；不自动重放动作 |
| timed_out / cancelled | deadline 或显式取消 | 状态查询，不能默认输入未执行 |
| outcome_unknown / idempotency_conflict | 执行结果不明或请求 ID 内容不同 | 不自动重放有副作用操作 |

错误 details 至少包含 operation_id（存在时）、目标 scope、失败阶段、可用的诊断/产物引用，以及 retryable。不得包含 token 或未筛选的环境变量。

## 12. 实施与测试落点

F02 负责类型、兼容和请求路由；O01 负责窗口/心跳；O02 负责资源 ACK；O03 负责产物；O04 负责同步编排；O05/O06 负责树和查询。契约测试覆盖旧 JSON fixture、未知字段、未知版本、乱序回复、游标投影和数据上限。

真实验证至少覆盖 [O-01–O-12](../roadmap/acceptance-matrix.md)、[C-01–C-05](../roadmap/acceptance-matrix.md)。只有 synthetic socket 测试不能证明截图、帧或 UI 输入可用。
