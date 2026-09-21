# 验收矩阵与实验操作单

状态：拟议测试规范，68 个用例均未因本文编写而实际执行。任务对应关系见 [实施清单](implementation-backlog.md)，设计入口见 [总路线](../ROADMAP-agent-native-development.md)。

## 1. 执行与证据约定

每个用例由“前置条件 → 明确动作/故障 → 可判定结果 → 证据”组成。测试 driver、fixture runtime 和下文的新命令都是后续任务的交付物；本文不是声明当前 CLI 已支持它们。

### 1.1 CI 层级

| 层级 | 环境 | 能证明什么 | 不能替代什么 |
| --- | --- | --- | --- |
| L0 | Linux/macOS/Windows 普通 runner，无 GPU | 解析、协议、状态机、统计、文件事务 | UI 真实响应/截图/输入 |
| L1 | 原生工具链、生成模板构建环境 | cargo check/package、宿主打包和依赖可解析 | GPUI 实际渲染及原生生命周期 |
| L2 | 有图形会话的 macOS/Windows/Linux 或 iOS/Android 模拟器 | 真窗口、输入、资源、截图、跨层关联 | 真机行为及精确 GPU 性能结论 |
| L3 | 固定硬件/电源/驱动和受控设备池 | 性能、GPU trace、真机、远程并发隔离 | 未验证的其他设备/后端 |

一个用例标 L0+L2，表示纯契约与真实行为都需要，不是二选一。缺机器记 not_run/unavailable，不能记通过。现有 Android native_probe.c 的小型 ELF 仅服务 L1 宿主打包。

### 1.2 Evidence root

拟议测试结果根为 `.gpui/acceptance/<commit>/<case-id>/<variant>/<attempt>/`，下文简称 `E/`。每次尝试独立存放，不能用 retry 覆盖首次失败。运行前记录该目录的绝对路径，清理只针对本次创建的子目录。

每例都必须保存：

- `case.json`：case_id、variant、task_id、expected、actual、verdict、开始/结束、失败阶段、是否重试、证据索引。
- `environment.json`：commit/dirty diff hash、依赖锁、host/runner/device、工具链、features/profile、后端/屏幕/字体/locale、实际能力。
- `commands.json`：argv、工作目录的去敏标识、退出码、deadline；环境只记录白名单/秘密键名，不记录凭据值。
- `events.ndjson` 或纯单元测试报告；适用时保存 `operation.json`、`observation.json`、`check.json`、`matrix.json`。
- 额外图像/树/trace/文件快照的内容 hash、长度、source/run/window/scene 身份；无法取得也必须记录原因。

测试结论与被测操作状态分开：故意触发 build_failed 且错误处理正确，测试 verdict 可以 passed，但 check/operation 仍必须 failed。重现某 bug 也不等于修复它。

### 1.3 故障注入规则

L0 使用受控 clock、transport、filesystem 和 command runner，精确在“build started / input dispatched / capture before publish”等屏障注入故障，不依赖碰运气的 sleep。L2 通过测试专用 feature 的明确开关触发真实 UI/资产错误，并同时保存平台证据。测试 hook 不进入正常 release，不允许假 screenshot/semantic tree 替代 GPUI 导出。

所有等待都有限时。人为阻塞测试必须提供由测试 supervisor 控制的独立退出路径，不能永久卡住用户窗口或进程。

## 2. 标准夹具与环境组合

| 夹具 | 初态与 logical_id | 可注入错误 | 最小必测环境 |
| --- | --- | --- | --- |
| Counter / counter-basic | 0；counter.value、counter.increment | disabled、遮挡、重复点击、旧 run、UI 阻塞、资源延迟 | macOS 真窗口；G3 后移动两端 |
| LoginForm / login-invalid | 空表单，fixture 返回 invalid_credentials；login.password、login.submit、login.error | 异步旧响应、请求中取消、键盘遮挡、未知语义、伪日志指令 | macOS；iOS/Android 键盘场景 |
| VirtualList / list-scroll | 1000 项，固定 row_height=32、item-0000 至 item-0999；list.viewport | 裁剪、重复 ID、丢失资源、过度重绘、大树 | macOS；固定硬件性能 runner |

fixture 结构见 [示例目录](../examples/README.md)。不同测试 variant 修改 fixture 必须生成不同 hash；不可在同一 hash 下改变预期数据。

每个平台至少记录 light/dark、默认/大字号、1x/实际高 DPI 中实际支持的组合；不支持请求 DPI 的平台回报 actual 值。基础闭环先固定 light/en-US/真实 DPI，扩展组合不能推迟最小夹具验收。

## 3. C：兼容、协议与发布

### C-01 · 版本协商和旧模板退化

- 前置/层级：F02；L0+L1。保存基线 CLI/v1 模板和新 CLI/v2 runtime。
- 动作：分别连接旧 CLI→新 supervisor、新 CLI→旧 supervisor、新 supervisor→v1 app、v2 app→旧 supervisor；再提交未知 schema/proto、错误 token。
- 预期：D1 status/diagnostics/events 保持 schema=1；新功能明确 upgrade_required/unsupported_version；v2→v1 只在版本拒绝时新建连接降级，认证失败不回退。
- 证据：`E/compatibility.json`、全部握手/响应去敏样本、旧二进制版本/hash；每个方向都单列结果。

### C-02 · Release feature 与依赖发布边界

- 前置/层级：F02/T04；L1，端口验证 L2。生成 debug、普通 release、gpui-profile 和非法 feature 组合。
- 动作：构建各组合；debug+gpui-dev 启动观察；release+gpui-dev、gpui-dev+gpui-profile 应编译失败；从 package 内容在干净目录生成应用。
- 预期：普通 release 无开发控制入口；profile 仅有受限显式测量能力；包依赖可从已发布版本/不可变 revision 取得，无开发者绝对路径。
- 证据：`E/features.json`、构建/链接结果、监听入口检查、`package-list.txt`、生成项目 manifest/lock；L1 编译不能独自证明无运行端口。

### C-03 · 在线 v1 投影和游标

- 前置/层级：F02；L0。统一 journal 混合 D1 与 v2 专属事件，含一整页仅新事件的情形。
- 动作：旧 CLI 从 seq=0 持续分页，再跟随新事件；发送超过内存保留量的事件并查询过期 cursor。
- 预期：新 kind 一对一映射为 debug app.log，占位保留 seq；无假 gap、无空页死循环、无虚假 runtime issue。真实淘汰返回 cursor_expired/resync，不填占位掩盖丢失。
- 证据：`E/canonical-v2.ndjson`、`projection-v1.ndjson`、`pages.json`；检查身份/seq 一一映射和上限。

### C-04 · 退出后的旧 archive reader

- 前置/层级：F02；L0，必须使用保存的旧 CLI 或未经修改的旧 reader。
- 动作：旧 CLI follow 时生成新事件、退出 supervisor；让 reader 从不同 cursor 恢复根目录日志；分别触发 v1/v2 日志轮转。
- 预期：根目录 events/state 为 v1 可读；投影不造成序号空洞；真正删除的 segment 才触发 gap。新 reader 只读取选定版本，不重复事件，不把两份日志的最早 seq 混用。
- 证据：`E/archive/`、旧 follow stdout/exit code、两版 earliest_seq、恢复事件 seq 列表。

### C-05 · 帧、路由、权限与输入上限

- 前置/层级：F02/O03；L0。鉴权 transport 和请求路由测试服务。
- 动作：发送 1MiB 边界/超限/截断帧、未知消息/字段、重复 transfer offset、乱序回复；用 app token 读控制产物、用别 session ID 读 artifact。
- 预期：按版本约定拒绝非法输入；回复只完成对应 request，终态不可被迟到消息改写；未认证不分配大缓冲/不访问文件，错误无秘密。
- 证据：`E/protocol-corpus.json`、内存/队列峰值、返回 code、连接清理状态；无 panic、死锁或跨 session 数据。

## 4. O：版本可信的观察

### O-01 · 编辑后的成功同步观察

- 前置/层级：O04；L2/macOS Counter。旧窗口显示文本 A，注册新资源和确定版本。
- 动作：将可见文本改 B，立即 observe --sync；并发查询 status/events；本次不依赖额外 watcher 事件触发。
- 预期：主动 scan 进入真实 build coordinator，成功观察绑定 B 的 source/build/run/window/revision；截图可见 B，UI responsive，assets applied。普通状态 P95 目标≤200ms。
- 证据：`E/before.png`、`after.png`、输入 manifest、操作/事件时间线；图片不是用新 metadata 标记的旧内容。

### O-02 · 构建中再次编辑

- 前置/层级：O04；L0+L2。构建存在可控屏障，目标 R1。
- 动作：R1 build.started 后写 R2，再释放构建；分别查询 R1 operation 和新的同步观察。
- 预期：R1 保持原目标并 superseded，不被自动改成 R2 成功；R2 有新操作和输入身份。重复到 build/launch 两个边界。
- 证据：`E/revisions.json`、两个 operation、build result、源文件 hash；wrong_revision_acceptance=0。

### O-03 · 编译失败但旧应用仍活着

- 前置/层级：O04；L2。先有成功 run，再引入明确语法错误。
- 动作：observe --sync，查询 diagnostics，读取旧截图；修复语法后再次观察。
- 预期：第一操作 build_failed/CLI 非零，诊断关联失败 build；旧 app 可查但 freshness=stale，不当本轮成功。修复后的结果使用新 run，历史错误仍可追踪。
- 证据：`E/failed-operation.json`、Cargo diagnostics、旧/新观察与图像身份。

### O-04 · 多窗口、迟到回复和旧运行

- 前置/层级：O01/O04；L0+L2。一个 run 两窗口，延迟第一窗口 capture；随后启动新 run。
- 动作：不选窗口调用 observe；选旧 window 调用；把旧 run 的 probe/capture 延迟回复送达。
- 预期：分别 ambiguous_window、stale_observation/target_exited；迟到数据不能完成新 run 操作。错误 token 的“同名 run”也不能接管。
- 证据：`E/window-lifecycle.ndjson`、全部 request/run/window 元组、响应关联检查。

### O-05 · UI 卡顿与休眠不是一回事

- 前置/层级：P01/O01；L2。Counter 可在 UI 线程受控阻塞 5s，网络线程保持响应。
- 动作：记录静态窗口正常 10s；阻塞 UI；再分别后台挂起、系统休眠/恢复、关闭窗口。
- 预期：静态不重绘仍 responsive；已知前台且未休眠时 3s probe 超时为 unresponsive；挂起为 suspended，无法确定为 unknown；恢复重置 deadline，关闭后无悬空请求。
- 证据：`E/probes.ndjson`、UI/网络线程时间、前后台/睡眠证据及恢复后的即时 probe。

### O-06 · ACK 不等于已经显示资源

- 前置/层级：O02；L0+L2。有必需图片 A，替换为 B；可分别延迟写入、缓存失效、图片解码。
- 动作：在每个阶段调用同步观察；再注入损坏图片和 hash mismatch。
- 预期：received、cache_invalidated、required_loaded 独立；必要图片未加载或 scene 未使用新 revision 时不能 assets=applied。失败有路径/阶段，截图不悄悄复用 A 宣称 B 已生效。
- 证据：`E/asset-transactions.json`、A/B 截图、ACK 时间与对应 scene_epoch。

### O-07 · 删除、部分传输和重连

- 前置/层级：O02；L0+L2。两个资源，只完成一半传输后断开应用通道。
- 动作：离线期间删除一个资源并替换另一个；重连；再丢失 commit ACK 重试。
- 预期：用 manifest 对账恢复，删除为显式条目，事务重试不重复副作用；无残留旧缓存冒充存在。缺必需资源明确失败，不因磁盘 read 失败静默跳过。
- 证据：`E/manifests/`、删除事件、重连前后资源目录和画面、transfer ID 关联。

### O-08 · 截图/树缺失和权限撤销

- 前置/层级：O03/O04/M01；L0+L2。正常窗口具备截图，测试适配器可声明/撤销某项能力。
- 动作：分别要求 screenshot、screenshot+semantics、scene-only；撤销截图权限、断开传输、只提供 device capture。
- 预期：必需缺失为 unavailable/permission_denied，不能拿空树/空 PNG 成功；可选 semantics 缺失可成功但 completeness=partial。scene-only 不退化 device_screen，已声明能力变化及时失效。
- 证据：`E/capabilities-before-after.json`、每种 require 的响应、传输错误和未发布产物清单。

### O-09 · 采集中再变化和操作终态

- 前置/层级：O04；L0+L2。采集前后校验点可暂停。
- 动作：第一次读图时改变 scene，再重试时改变 scene；另一个 variant 只改源文件，或在 publish 前关闭窗口、取消操作。
- 预期：scene 最多重试一次，再次不稳为 capture_unstable；目标源码改变为 superseded；关闭/取消按明确终态结束。迟到 publish 不把 failed/cancelled 改 succeeded。
- 证据：`E/capture-attempts.json`、每次前后 scene/input hash、最终 operation 及临时产物清理。

### O-10 · Scene、设备截图与呈现证据

- 前置/层级：P01/O04/O05/M01；L2，各 provider 分开。
- 动作：交替显示带 scene 编号的两帧并采图/树；分别使用 scene readback、OS window 和 device screenshot。
- 预期：same_scene 的图和树编号一致；外部截图只标可证明的 best_effort，包含捕获时间/前台应用/范围。没有 backend 呈现通知时 presented_frame_id=null，on_next_frame 不可替代。
- 证据：`E/frame-correlations.json`、图像/树/回调顺序、GPU fence/读回生命周期；窗口关闭和设备丢失子例无悬挂。

### O-11 · 语义、虚拟化、分页和差异

- 前置/层级：P01/O05/O06；L0+L2。Counter/Form/List，屏幕阅读器未开启；另有 50k 节点压力输入。
- 动作：导出 ID/role/value/bounds/clip/focus；滚动 List 再查询；遍历分页；修改/删除节点，拿旧 cursor 和重复 logical_id 查询。
- 预期：节点字段来自真实 GPUI，未知明确 null/unsupported；不导出未实例化列表项。每页≤200 节点/128KiB；游标绑定快照和过滤条件；不能用临时别名匹配跨观察节点。
- 证据：`E/trees/`、`pages.json`、`diff.json`、与实际布局对照；超限/过期/多候选有明确结果。

### O-12 · 观察与存储资源上限

- 前置/层级：O03；L0，真实大图子例 L2。
- 动作：并发第 5 个 observe、第 3 个 transfer、第 65 条 UI 请求；上传超 16MiB/20MP PNG、超 50k/16MiB 树；填满 session/project 配额，pin 部分产物后清理。
- 预期：按各层 busy/quota_exceeded/明确超限拒绝，无无限分配；probe 可合并但输入不丢。活动/pin 不回收，空间不足就失败，过期 ID 返回 artifact_expired。
- 证据：`E/resource-peaks.json`、已接纳/拒绝数、磁盘索引、临时文件与引用完整性；读回哈希损坏为 checksum_mismatch。

## 5. S：场景与交互

### S-01 · 配置校验早失败

- 前置/层级：S01；L0。合法三场景与非法变体 corpus。
- 动作：加入未知字段/重复 ID/0 或 201 步/超 deadline/越界 fixture/错误 JSON/非法 viewport/未知 component/不匹配 expected 类型。
- 预期：启动应用前有定位到字段/行的错误；unknown registry 不等于已检查 component；fixed clock 缺 clock_at 拒绝，无效 step 不被跳过。
- 证据：`E/schema-corpus.json`、规范化结果/hash、零 app launch 计数。

### S-02 · Reset 与异步任务隔离

- 前置/层级：S02/S04；L2。Counter 先人为点击至 7，LoginForm 保留一个延迟请求。
- 动作：连续 20 次从 fixture 执行 check；在 reset 后送达旧请求；有/无旧 Live snapshot 各跑一次。
- 预期：Counter 总从 0 开始，reset_generation 递增；旧异步响应不影响新场景，snapshot 不自动导入。进程内 reset 未证明可靠时用新进程。
- 证据：`E/reset-generations.json`、每轮初始观察、旧请求取消/隔离证据及 run/data-dir 列表。

### S-03 · 输入实际经过正常事件路径

- 前置/层级：S03/S04；L2，三个夹具。
- 动作：Counter click 一次，Form 输入文本/Enter，List scroll 到 item-0042；记录正常输入事件而非业务 handler 测试回调。
- 预期：计数 1、表单出现指定错误、目标项可查询可见；focus/hit-test/事件冒泡行为符合真实应用。action.finished 后另做观察断言，不能把 dispatch 完成当业务成功。
- 证据：`E/input-events.ndjson`、每步前后图/树和 check.json；至少一个事件拦截变体证明未绕过命中路径。

### S-04 · Disabled、遮挡和离屏节点

- 前置/层级：S03；L2。Counter disabled、覆盖透明阻挡层、List 目标项离屏三个 variant。
- 动作：对目标节点 click，再解除条件后重新 observe/click。
- 预期：分别 element_disabled/element_obscured/element_not_visible 或尚未实例化时 selector_not_found；错误步骤不增加计数。不允许直接 invoke handler 绕过。
- 证据：`E/hit-tests.json`、遮挡/clip 图和节点状态、动作前后业务值。

### S-05 · 旧 selector 与坐标转换

- 前置/层级：S03；L0+L2。记录观察，随后替换控件或重启 run，另测实际 2x/旋转窗口。
- 动作：用旧 node_ref/observation 点击；用绑定正确图像尺寸/DPI 的坐标点击；改 DPI 后重复旧坐标请求。
- 预期：旧引用 stale_observation，不因 logical_id 相同跳过预检；有效坐标落在正确控件，记录转换；图像/方向不匹配拒绝。
- 证据：`E/coordinate-transforms.json`、before/after scope、命中事件，无错误窗口副作用。

### S-06 · 幂等、缓存过期与未知结果

- 前置/层级：S03；L0+L2。request_id=r1 的 click，可在投递/回包间断线。
- 动作：相同 ID/参数重试 10 次；同 ID 改参数；用受控时钟淘汰结果再重试；分别填满 10000 项或4MiB 墓碑上限；投递后崩溃并恢复。
- 预期：计数最多加 1；不同参数 idempotency_conflict；结果过期或执行不明为 outcome_unknown。墓碑满拒绝新 ID，不淘汰来重放；新 run 不执行旧请求。
- 证据：`E/idempotency-journal.json`、调用/投递次数、run/action/result 状态；L2 实际断线至少验证一次。

### S-07 · 断言三态与歧义

- 前置/层级：S04；L0+L2。缺 semantics、重复 role/name、unknown enabled、有历史 error 的场景。
- 动作：执行 absent/enabled/text_equals/no_runtime_errors，并对多匹配 selector 请求动作；再生成本轮允许的业务错误与未允许 panic。
- 预期：缺能力/字段不是 absent/passed；多匹配 ambiguous_selector；历史错误保留但 no_runtime_errors 按本轮 seq 范围判定。允许规则必须精确，不吞 panic。
- 证据：`E/assertions.json` 的 expected/actual/reason/seq 范围及有限候选列表。

### S-08 · 多客户端和人工输入污染

- 前置/层级：S03/S04；L2。check 持窗口执行权，另一个客户端只读观察。
- 动作：另一客户端插入 click/reset；用户在真实窗口输入；执行取消，随后重新从 fixture 检查。
- 预期：只读不受阻，另一个动作 owner 被拒；人工输入被检测则 contaminated/inconclusive，不覆盖用户操作。不能检测外部输入的平台必须披露不能满足严格隔离。
- 证据：`E/owners.json`、输入来源与污染事件、取消/释放、下一轮干净初态。

### S-09 · 环境/fixture 确定性边界

- 前置/层级：S01/S02/S04；L0+L2。固定 seed/业务 clock、fixture 网络；另有不支持某 locale/DPI 的 adapter。
- 动作：相同 fixture 连续运行；切换 theme/locale/seed/fixture 内容；请求不支持的 fixed clock 与实际 DPI。
- 预期：支持的适配真实生效且 hash 变化可追踪；不受控网络/随机/OS 动画列入 uncontrolled_inputs。请求/实际环境不符时严格视觉 not_comparable，不能声称全系统时钟被冻结。
- 证据：`E/requested-actual-environment.json`、fixture/schema/hash、初始化与 ready 数据。

### S-10 · 热参数版本与源码固化

- 前置/层级：S05；L2，显式注册至少 3 类参数。
- 动作：20 次颜色/间距/字号修改，包含超范围、撤销、重启；生成固化 patch 后并发编辑源码再申请应用。
- 预期：非法参数拒绝；观察带 overlay_revision/preview_only；清除后归零。源码 hash 变更导致冲突，不能覆盖；非零 overlay 不能作为已提交实现的验证。
- 证据：`E/overlay-history.json`、截图、patch 与源 hash、相对重建延迟；无收益也如实记录 reject。

## 6. M：平台、租约与执行矩阵

### M-01 · iOS simulator 真实捕获/日志

- 前置/层级：M01；L2/macOS，完整 GPUI iOS simulator 应用，显式 UDID。
- 动作：启动、截图、旋转、唤起键盘/权限弹窗、切到另一 app；注入早期 native crash 和仅通道断开。
- 预期：设备截图二进制完整，方向/系统 UI/scope/前台身份准确；其他 app 不被标目标 screenshot。native crash 有关联日志；断线只改 channel，无退出证据不宣布 exited。
- 证据：`E/simctl-commands.json`、PNG、native/app 日志、进程/通道时间线；不得把结果推广到 iOS 真机。

### M-02 · Android 多设备、ABI 与 native logs

- 前置/层级：M01；L2，至少一个实际运行 GPUI 的 emulator，第二 serial 可用来检测误选。
- 动作：指定 serial 安装/启动/exec-out screencap；注入 JNI/renderer early crash、ANR、PID 重启/重用；尝试不匹配 ABI。
- 预期：命令始终带目标 serial，图片不经文本转码；日志绑定包/PID/启动身份，不归属的单列。ABI 不匹配在安装前失败，不把另一个设备日志归给当前 run。
- 证据：`E/adb-commands.json`、APK ABI manifest、PNG、logcat/crash 与关联置信度；L1 stub APK 不满足本例。

### M-03 · Required/optional 矩阵聚合

- 前置/层级：M02；L0+L2。三端 Counter/Form/List，另有无 runner 的 Windows cell。
- 动作：分别设 Windows required/optional；让一格断言失败、一格缺语义、一格超时；切换 fail_fast。
- 预期：required 全 passed 才整体 passed；optional 缺失整体 partial，不计为已验证平台。failed/inconclusive/unavailable/cancelled 区分；fail_fast 保留已完成证据和未执行原因。
- 证据：`E/matrix-variants/`、各 cell 独立 check、同一 frozen hash、聚合 golden outputs。

### M-04 · 跨项目租约竞争和 TTL

- 前置/层级：M03；L0 三 OS 真进程，移动实际排他子例 L2。两个项目/worktree 同设备 ID。
- 动作：A 持锁，B 请求；停止 A heartbeat 超过 30s 但进程/OS 锁仍活着；再正常释放或终止 owner。
- 预期：B 得 device_busy，suspect 不允许抢锁；只有锁释放并确认 owner 失效才恢复 metadata。不同项目目录不能获得两个写 owner。
- 证据：`E/lease-processes.json`、OS 锁/heartbeat/fencing 时间线、B 零安装/输入记录。

### M-05 · Fencing、断连与进程身份

- 前置/层级：M03；L0+L2。旧 lease=t1，设备断连后同 serial 重新出现并取得 t2。
- 动作：投递旧 token 的迟到安装/点击/cleanup；模拟 PID 重用、metadata 残留和 supervisor 崩溃。
- 预期：旧 token 拒绝，不修改新 run；PID 字符串相同不足以认领进程。资源必须重新探测，旧 metadata 不自动授权删除。
- 证据：`E/fencing.json`、进程启动身份、拒绝次数与实际设备状态。

### M-06 · 取消和资源所有权清理

- 前置/层级：M03/M02；L0+L2。用户启动一个模拟器，测试另建一个临时设备，并有并行独立作业。
- 动作：分别在租约等待、构建、安装、输入已投递、日志排空时取消/超时/杀 supervisor。
- 预期：清理只作用于 owned resources；不关闭用户设备，不杀其他作业。已投递未知输入不虚报未执行，有限时排空，租约最终释放或明确需恢复。
- 证据：`E/owned-before-after.json`、process tree、lease、取消终态、保留产物。

### M-07 · 冻结快照与外部输入

- 前置/层级：M04；L0+L1。workspace 包含外部 path dependency、native 配置、fixture/assets。
- 动作：建快照时改文件，快照后再改工作目录；加入越界 symlink/循环、未声明 build.rs 输入和含秘密文件。
- 预期：快照内容/hash稳定，所有 cell 使用同一清单；构造期变化有界重试或 unstable_inputs。声明外部根正确重定位，越界/缺输入明确拒绝/不可复现，秘密与整个 home 不被收集。
- 证据：`E/source-manifest.json`、源/快照内容对照、untracked_inputs、原目录未改动证明。

### M-08 · Profile/ABI/构建键隔离

- 前置/层级：M04/M02/T06；L0+L1，实际安装 L2。
- 动作：并发 debug/release/profile、arm64-v8a/x86_64，修改 features/native config/锁文件/编译环境；两个调用共享相同 key 后取消其中一个。
- 预期：每个完整 BuildKey 对应独立 staging，JNI/DerivedData 不串写；共享仅限相同 key，取消引用不终止他人构建；安装仍生成新 run。
- 证据：`E/build-keys.json`、APK 内 ABI/产物 hash、staging 路径与构建引用计数。

### M-09 · 远程握手、授权和传输

- 前置/层级：M06；L0+L3，两台独立机器且 host 显式登记。
- 动作：握手版本不符、工具链/架构不符、传输缺块/坏 hash/路径越界、host 未登记、配额满；最后跑正常 job。
- 预期：不隐式联网找替代 host，不运行未经校验输入；错误在适当阶段报告，remote path 不在客户端直接打开；只有有效认证通道可操作设备。
- 证据：`E/runner-handshake.json`、transfer manifest、拒绝响应、正常远端 job 的环境和结果。

### M-10 · 远程重连不重复执行

- 前置/层级：M06；L0+L3。作业已点击但响应尚未到达，或只读捕获仍在执行。
- 动作：断链、用同 job/request 重连查询，过保留期后再次重连，发送旧 fencing_token。
- 预期：不重装/重复点击；仍保留的结果可恢复，到期为 terminal/outcome_unknown，旧 token 拒绝；任务拥有的进程/租约有界清理。
- 证据：`E/reconnect-transcript.json`、服务端执行计数、远端清理前后资源；两个客户端声明同 job 不产生双 owner。

## 7. R：复现与视觉基线

### R-01 · 导出完整性与隐私

- 前置/层级：M05；L0+L2。失败 check 包含假 token、敏感文本、截图 mask、源码定位和普通日志。
- 动作：默认 export，再分别 include-source/include-patch；读取 manifest 与每项 hash。
- 预期：默认无源码、patch、token/签名/全环境；敏感文本筛除、截图按明确 mask 处理，重算 artifact/hash 并标 redacted。无法可靠筛除的类别默认不导出并说明。
- 证据：`E/export-inventory.json`、筛除前后 hash、仅测试凭据的泄漏扫描结果；重放前置条件完整。

### R-02 · 恶意/损坏归档拒绝

- 前置/层级：M05；L0，构造 ZIP corpus，不执行包内内容。
- 动作：输入 ../、绝对路径、symlink、重复路径、大小写/Unicode 碰撞、错误 hash、超 10000 文件/512MiB、压缩炸弹和嵌入恶意 HTML。
- 预期：在安全预算内拒绝且不写出目标目录，不入正常索引；报告转义 HTML；inspect 不运行 shell/JS/二进制。
- 证据：`E/archive-corpus.json`、沙盒前后目录清单、峰值资源、零外部进程/网络请求。

### R-03 · 缺源码与不可信重放

- 前置/层级：M05；L0+L1。只有诊断包、私有依赖缺失包、带源代码包三类。
- 动作：inspect 后直接 run，或指定不匹配 source_hash 的项目；提供正确可信源码再执行。
- 预期：分别 requires_source/prerequisite_missing/source_mismatch，不凭 base commit 猜当前工作区；执行需要显式 run 和目标，不自动下载未知可执行文件。
- 证据：`E/replay-prerequisites.json`、源码验证、实际执行 argv；inspect 阶段零执行。

### R-04 · 跨干净工作区复现失败

- 前置/层级：M05/Q01；L2。可重复的 Form 错误，已知输入/设备配置。
- 动作：导出，移入独立干净工作区，inspect/run；再应用受控修复，用同场景重跑。
- 预期：首次在相同步骤重现同一断言失败，有新 run/独立产物；修复后新结果 passed。reproduced 与 fixed 是不同字段，不伪造原 run ID。
- 证据：`E/original/`、`replayed/`、`fixed/`、输入/环境可比报告、差异说明。

### R-05 · 视觉基线不能自动吞失败

- 前置/层级：S04/M02/M05；L0+L2。固定 viewport/DPI/font/theme 的已批准基线。
- 动作：制造裁剪/颜色差；移除基线、改变 DPI/locale/scope、尝试在失败后自动扩大 mask 或更新基线。
- 预期：明确 failed+diff、baseline_missing 或 not_comparable；基线更新只能独立 review，保留旧/新/diff/批准记录。行为断言优先，跨平台字体不做盲目逐像素比较。
- 证据：`E/baseline-key.json`、算法/容差/mask版本、diff.png、审批/拒绝记录。

## 8. P：延迟、性能与 GPU

### P-01 · 开发链路 span 的真实性

- 前置/层级：F01；L0+L2。正常编辑、语法失败、取消、superseded、native 安装失败。
- 动作：每类执行并记录各阶段；模拟设备时钟偏移，构建输出同时带日志。
- 预期：以 supervisor 单调时钟计接收/编排，设备原时间单列；父子 span、ID、终态闭合，无负时长。失败也入样本，不用 stdout 行距伪造逐 crate CPU 耗时。
- 证据：`E/spans.ndjson`、stage totals、故障事件/实际命令关联。

### P-02 · 基线、尾延迟和扫描/缓存提速

- 前置/层级：F01/T05/T06；L2，固定小夹具与大型输入集，记录硬件/缓存。
- 动作：10 预热+30 测量，构建中并发 status；比较原实现、索引、缓存，保留失败/超时/superseded。
- 预期：报告 P50/P95/样本数和每阶段分布；G1 小夹具目标 status≤200ms、probe≤500ms、无需构建 observation≤2s（均 P95）。达不到记录瓶颈/预算变更，不修改样本选择。
- 证据：`E/latency-raw.json`、`comparison.json`、缓存命中原因与扫描 bytes；这些预算不能被写成所有机器承诺。

### P-03 · CPU/GPU/帧指标有可验证来源

- 前置/层级：P01/G01；L0+L3。有已知 CPU 布局负载、GPU 工作负载和正常对照。
- 动作：独立改变 CPU/GPU 负载；禁用 timestamp query，注入 disjoint/无效查询和设备丢失。
- 预期：CPU paint、frame interval、GPU duration 分别变化并标 source；无 GPU 能力则 unavailable，不填 CPU 估计。present_latency 无呈现通知时不可用。
- 证据：`E/metrics-source.json`、hook/外部工具对照、无效样本数、原始帧 scope。

### P-04 · Instrumentation 开销与有界采样

- 前置/层级：G01；L3 固定硬件、相同 profile/fixture，只有 instrumentation 开关不同。
- 动作：交替执行开/关，各至少30次；压满采样队列、延迟消费、保持运行10min。
- 预期：CPU 帧 P95 增量预算≤5%；超出则记录并降低/关闭采样。drop 可见，run 标 incomplete；内存/磁盘不随无限帧增长。
- 证据：`E/overhead-pairs.json`、统计区间、队列/内存曲线、默认采样设置决策。

### P-05 · 统计规则、零基线与边界

- 前置/层级：G02；L0，固定 seed 的已知分布：明显正常/退化/跨阈值/缺样本/零基线。
- 动作：各配置 absolute-only、relative-only、both；使用30 run/30 pairs和2000 bootstrap，打乱顺序后重算。
- 预期：绝对下界>limit才失败，上界≤limit通过；相对按百分比+min_delta+配对区间判断，区间不确定为 inconclusive。任一规则失败整体失败；零基线不除零/不删除规则。
- 证据：`E/statistics-fixtures.json`、算法版本、expected/actual 聚合与区间；相同 seed 重跑一致，坏单位/少样本明确拒绝。

### P-06 · 不可比环境与正确性先行

- 前置/层级：G02；L0+L3。两个 GPUI/profile/backend/DPI/电源模式不同的结果；一组“更快但漏绘”候选。
- 动作：compare 不同键/不同日期未配对数据，模拟热降频/样本缺失，再运行正确性断言。
- 预期：环境不符 not_comparable，缺配对不声称 paired CI，未知环境/缺样本 inconclusive；漏绘场景即使更快也不能优化通过。
- 证据：`E/comparability.json`、环境差异、pair_id、正确性 check 和趋势/正式结论区分。

### P-07 · GPU provider 不可用时的诚实退化

- 前置/层级：G03；L0+L3。Metal 环境、无 gpudebug 的工具链、无权限和 API 不支持四类。
- 动作：请求 RenderDoc 捕获 Metal、请求缺失 gpudebug、撤销 capture 权限；随后正常 observe/check。
- 预期：明确 backend_unsupported/tool_not_found/permission_denied，含人工准备建议；不自动安装/切换工具链，普通开发循环仍可用。
- 证据：`E/provider-probes.json`、实际探测退出码/版本、降级响应，无伪 trace。

### P-08 · GPU capture 身份、分析引用和清理

- 前置/层级：G03；L3，至少一个实际支持 provider。
- 动作：正常/渲染错误/负载退化各捕获目标 scene 区间；重启 run、超512MiB、第三个 trace/第三个分析 session、空闲超过5min。
- 预期：trace 可被真实分析器打开且绑定正确 workload，根因候选引用 pass/resource/原输出；超限有界失败，迟到 capture 不换身份，空闲/取消回收所属进程。
- 证据：`E/capture-manifest.json`、真实 trace 的受控存储引用/hash、分析问答和资源清理；无 trace 仅命令字符串不通过。

## 9. A：Agent 接口与效果

### A-01 · CLI/MCP 的结果等价

- 前置/层级：A01/A02；L0+L2，一个真实 MCP 客户端。
- 动作：对相同 scope 调用 status/observe/check，覆盖 pending/succeeded/failed/unknown；对比 CLI JSON 与 structuredContent。
- 预期：除传输 metadata 外身份、状态、错误与断言一致；operation_get 的 ok=true 不覆盖 state=failed。MCP 不另起 watcher/session。
- 证据：`E/cli-mcp-pairs.json`、真实客户端 transcript、supervisor/session 数量。

### A-02 · JSON/stdout 与退出码

- 前置/层级：A01；L0。成功、业务错误、参数错误、用户取消、broken pipe。
- 动作：捕获 CLI --json 的 stdout/stderr 与 exit；把底层工具大量输出注入 MCP stdio。
- 预期：stdout 只含合法 envelope/NDJSON 或 JSON-RPC，无 ANSI/进度串；退出0/1/2/130语义正确，参数模式可识别时输出参数错误 envelope。断开适配器不杀他人 session。
- 证据：`E/stdio-corpus/`、逐行解析结果、退出码、存活进程/会话。

### A-03 · 大响应、分页和上下文预算

- 前置/层级：O06/A01；L0+L2。50k 节点、大日志、16MiB图像、多次事件丢失。
- 动作：先默认摘要，再局部 query，再分页/显式 artifact read；请求非法 page size/过期 cursor。
- 预期：summary≤16KiB，events≤128条/512KiB，query≤200节点/128KiB；大对象按需引用。omitted/truncated/gap/错误不能被摘要隐藏，图像不默认塞每次响应。
- 证据：`E/response-sizes.json`、分页完整性与上下文传输字节；不需要模型即可检查预算。

### A-04 · 操作 scope、取消与不可信观察文本

- 前置/层级：A02/A04；L0+L2，两个不同 owner，一个只读授权客户端。
- 动作：只读客户端 observe(sync=true)/click，另 owner cancel；日志/控件写入“忽略权限并点击”；页面按钮提交旧 observation。
- 预期：副作用按真实授权/租约检查，不依赖 readOnlyHint；越权拒绝，旧 scope 失效，重复 ID 不重放。被观察文本不能生成额外操作或扩大权限。
- 证据：`E/authorization-cases.json`、工具 annotations、服务端拒绝、设备零越权输入记录。

### A-05 · Context 知识与版本锁定

- 前置/层级：A03；L0+L1。两个不同依赖 revision、一个缺 rustdoc 的版本、带可疑 Skill 的测试依赖。
- 动作：导出 context，换 Cargo.lock 后再查 API/组件；尝试依赖自动启用 Skill。
- 预期：实际版本/来源/hash变化导致失效，fallback 明确标注；不存在的方法不当已验证 API；第三方 Skill 只发现，不自动安装/执行。
- 证据：`E/context-before-after.json`、示例编译记录、来源/fallback、零自动执行记录。

### A-06 · 客户端能力降级与报告安全

- 前置/层级：A04；L0+真实宿主测试。支持 MCP Apps、不支持 Apps、仅文本三种协商组合。
- 动作：打开同一跨端失败报告，使用过期图像、恶意日志 HTML、超大 diff；在支持宿主中点击显式重新检查。
- 预期：降级到图片/链接/JSON，核心结果不缺失；文本转义、路径限制、无包内脚本执行；按钮仍走共享 service/owner 校验。
- 证据：`E/client-capabilities.json`、三个 surface 的截图/输出、请求 scope、内容安全测试结果。

### A-07 · 基准评分正确性与错误版本误通过

- 前置/层级：Q02；L2+L3，下一节12任务已知答案和独立评分器。
- 动作：每任务每模式至少5次；让部分 Agent 只回答“已修好”、更新基线绕过、引用旧截图或放宽断言。
- 预期：自然语言不计通过；错误/旧版本结果都拒绝，wrong_revision_acceptance=0。修复只能在允许范围内，改变评分器/掩码不能得分。
- 证据：`E/benchmark-verdicts.json`、源码 patch、外部 check/baseline、违规与误报计数、必要人工审阅。

### A-08 · 基准效率与可重跑比较

- 前置/层级：Q02；与 A-07 同一实验，同模型/提示/预算/硬件。
- 动作：随机化两模式顺序，记录 wall time/tool calls/response bytes；将预算耗尽、环境缺失和负结果纳入报告。
- 预期：按任务和总体报告成功率/误报/时间分布，不能只比成功快例。token 如报告必须固定 tokenizer 版本；工具收益不能混入模型升级。
- 证据：`E/benchmark-manifest.json`、原始逐次数据、评分器版本、统计脚本/结果；没有显著收益也提交结论。

## 10. T：工具链、升级、索引与缓存

### T-01 · Doctor 只阻断选择目标的必需项

- 前置/层级：T01；L0+L1，desktop-only 项目缺 Android/Xcode，另有 Android/iOS 项目。
- 动作：分别显式 target、按项目默认、非项目目录运行 doctor。
- 预期：desktop 不因未选移动工具缺失失败；所选目标 required 缺失退出1，可选 capture 缺失 warning；非项目报告 host 和平台 availability。
- 证据：`E/doctor-targets.json`、退出码、required/optional 列表、零配置修改。

### T-02 · 找得到命令不等于能用

- 前置/层级：T01；L0，可执行 shim 分别返回非零、畸形版本、挂起、大 stderr。
- 动作：替换 probe 的测试命令路径，执行各 shim；至少一个真实工具进行对照。
- 预期：退出/解析/timeout 各有原因；单检查5s/总30s预算，无无限等待和无界 stderr；子进程正确回收。
- 证据：`E/probe-results.json`、单调耗时、子进程清单、错误裁剪标记。

### T-03 · SDK/JDK/ABI 与秘密筛除

- 前置/层级：T01；L0+L1。错误 NDK 目录、缺 build-tools、JDK 与项目 AGP 不符、ABI/设备不匹配、含测试秘密环境。
- 动作：按 Android/iOS 真需求检查，不自动安装；同时测试多个候选设备。
- 预期：给参数数组形式修复建议和准确 expected/actual；多设备需要选择；不打印秘密值、不改 SDK/许可/签名，权限未知不当 pass。
- 证据：`E/toolchain-variants.json`、筛除检查、实际工具版本与文件存在性。

### T-04 · 模板基线可取得、可识别

- 前置/层级：T02/T03；L0+L1。新模板、添加平台、用户改共享文件、两个可能旧版本和完全自定义旧项目。
- 动作：生成/增平台/识别基线，清空 base 内容缓存后尝试 plan；检查 gitignore 跟踪结果。
- 预期：manifest 文件组/hash可复验，基线内容可按发行摘要取得；不把当前用户修改登记成 base。歧义为 manual_migration_required，缺内容 baseline_unavailable。
- 证据：`E/template-manifests/`、发行摘要/base 对照、`git check-ignore` 结果；Live token 仍忽略。

### T-05 · 三方合并与格式保留

- 前置/层级：T03；L0。B/L/N 覆盖双方不变、单方改、同改、冲突、新文件碰撞、删除；Android ABI/signing/renderer patch 夹具。
- 动作：只执行 plan，比较逐文件建议和项目目录 hash；测试 TOML 注释/顺序/用户字段。
- 预期：严格按三方规则，注释/签名/自定义 ABI 不丢，逻辑组不可局部成功；plan 不改受管理文件，Rust/Swift/Java 双方改默认冲突。
- 证据：`E/merge-corpus.json`、before/after目录摘要、human diff、plan JSON。

### T-06 · 多文件事务崩溃恢复

- 前置/层级：T04；L0+L1，可注入每个 journal/备份/replace/manifest/验证点崩溃。
- 动作：遍历这些点终止 apply，再运行 recover；注入磁盘满、备份失败、验证失败和没有平台工具链。
- 预期：从精确 journal 恢复到完整旧组或保留明确 recovery_required，不把半升级作为新基线；验证 not_run 不冒充全平台通过。
- 证据：`E/transactions/`、各步文件 hash、恢复后 manifest/构建结果、备份保留清单。

### T-07 · 并发编辑与回滚不覆盖用户

- 前置/层级：T04；L0。生成 plan 后改源文件；另一次在事务写完一个文件后人为编辑该文件。
- 动作：apply 第一个过期 plan；让第二事务失败再 recover；同时发第二 upgrade。
- 预期：plan hash 不符拒绝；回滚只恢复当前仍是本事务新 hash 的文件，用户新修改保留并报告冲突；升级锁阻止并发事务。
- 证据：`E/concurrent-edits.json`、三个版本 hash、保留用户 diff、锁与恢复状态。

### T-08 · 索引不能信任 mtime

- 前置/层级：T05；L0+L2。保存文件但大小/mtime不变、原子替换、目录 rename、事件丢失/overflow。
- 动作：每变体后 observe --sync 并比较全量 hash oracle；同时测增量扫描。
- 预期：无漏修订；第一版 sync 完整核验，overflow 回退；读取中反复改变有界 superseded/unstable_inputs。正确性不换取速度。
- 证据：`E/index-vs-oracle.json`、实际扫描文件/字节、最终源/运行匹配。

### T-09 · 外部依赖、链接和输入范围

- 前置/层级：T05/M04；L0+L1。外部 path dependency、链接循环、native config、README被build.rs嵌入、网络文件系统可选。
- 动作：逐一修改这些内容，重扫/冻结；测试未经允许的外部路径。
- 预期：已声明输入修改使 key 失效，不能按扩展名漏掉嵌入输入；循环/越界拒绝，不遍历整个 home。未知读集明确限制，网络盘不可靠时回退。
- 证据：`E/input-scope.json`、dependency 根/分类、hash oracle、拒绝/降级原因。

### T-10 · 缓存键、投毒和预热预算

- 前置/层级：T06；L0+L1。正确缓存、缺文件/坏hash/失败产物、不同profile/ABI/toolchain/env、共享构建。
- 动作：查询命中，损坏条目，再并发前台/预热、取消部分订阅者；活动条目清理。
- 预期：只命中完整且同键产物；投毒拒绝/重新构建，取消不杀他人；前台优先、预算有上限，活动文件不删，安装run不复用。
- 证据：`E/cache-decisions.json`、产物 hash/引用、前后台排队与资源峰值、提速的实际阶段时间。

## 11. P01 真实 macOS PoC 操作单

1. 保存本机 `rustc -Vv`、`cargo -V`、`xcodebuild -version`、`sw_vers` 和 GPU/显示器信息；记录 `src/template.rs` 中实际锁定 revision。把缺工具视作环境问题，不先升级所有依赖。
2. 在 `mktemp -d` 创建的实验目录生成 macOS-only Counter；当前可用入口是 `gpui init <name> --targets macos --path <dir>`、项目内 `gpui run desktop --live`。F01 的 Form/List 夹具准备好后使用相同依赖运行。
3. 编写实验 adapter，只做窗口注册/一个 UI probe/一张截图/一次树导出。用序号/颜色区分两帧，在画面和树中记录相同标记。保存所有源码修改；不把临时补丁隐藏在 Cargo 缓存中。
4. 比较普通 feature 和必要 test-support 的可编译性、依赖体积；在没有屏幕阅读器的正常桌面验证语义是否可主动启用。
5. 阻塞 UI 5s，保持 transport 活动；关闭窗口/重启应用；验证 probe 和 readback 的终止路径。无 device-loss 注入能力时该项 not_run，不编造结果。
6. 对每个 capture provider 回答：哪一个 scene？什么时候冻结树？readback 是否等 fence？是否证明屏幕呈现？捕获范围包含哪些系统 UI？
7. 记录至少30次捕获的时间与队列/内存，提交截图/树/hash/原始 trace 和 adopt/limited_adopt/reject。
8. 若需要上游接口，最小提案列出版本、签名草图、线程/生命周期和测试；未获接口前，O05 不能“以某个方法应该存在”为前提开工。

本操作单只允许修改专用实验夹具。不要为实验自动开权限、安装 SDK、切换 Xcode 或关闭用户其他应用。

## 12. Q02：12 项 Agent 基准任务

每项定义固定初始 commit/故障 patch、只允许修改的业务文件、场景及评分器 hash。默认每次尝试预算20min/100次工具调用，达到任一即停止并保留结果；如模型支持 token预算则另行固定。真实运行前可以调整预算，但两组必须一致并在实验 manifest 中预先登记。

| 任务 | 注入问题 | 独立通过判据 | 主要验证能力 |
| --- | --- | --- | --- |
| B01 | Counter Rust语法错误 | 对应 build 通过，counter-basic 通过 | 版本化编译诊断 |
| B02 | 使用不属于锁定版本的 GPUI API | 锁定依赖不变，替换为实际可编译 API | context 版本知识 |
| B03 | 大字号下列表行被裁剪 | list-scroll 在规定字号下 not_clipped | bounds/clip 与真实场景 |
| B04 | Counter disabled 条件写反 | 禁用态拒绝输入、启用态恰加1 | 输入前置检查 |
| B05 | Form 旧异步响应覆盖新请求 | reset 后新状态正确，取消无迟到污染 | 场景/fixture隔离 |
| B06 | 成功旧截图晚于失败新构建返回 | Agent 不宣称新版本通过，修复后有新观察 | source/run/operation绑定 |
| B07 | 删除或替换资源后仍显示旧缓存 | ACK与scene匹配且新图/删除结果正确 | 资源事务 |
| B08 | Android或iOS键盘遮挡提交按钮 | 指定移动目标真实键盘场景通过 | 原生平台证据 |
| B09 | Android宿主ABI与Rust库不一致 | 正确ABI APK完整且目标设备启动 | doctor/构建键/native宿主 |
| B10 | app通道建立前发生native crash | 定位正确进程日志、修复且重新启动通过 | 原生日志归属 |
| B11 | VirtualList引入可控过度重绘 | 行为仍通过，固定硬件预算通过 | CPU/GPU性能与正确性联合 |
| B12 | 所选目标缺工具链，另平台可用 | 准确报告环境阻塞且不误修业务/误报通过 | target-aware doctor |

B12 的正确结果是诚实定位阻塞，不要求 Agent 未经用户选择安装工具链。B11 仅在 G02 与固定硬件可用时进入正式效果汇总；此前整项 not_run，不以 mock 性能结果冒充。

实验至少12任务×2模式×5次=120次尝试；按任务随机化模式顺序。普通CLI模式不提供新结构化观察/场景接口，但两组允许相同业务代码编辑工具和目标环境。评分器/预算/答案不暴露为可编辑项目内容，失败包和全部原始数据保留供审查。

## 13. 发布时如何引用验收

每次发布在能力表中填写：平台/版本/provider → task ID → case IDs/variants → evidence hash → CI job/人工受控运行记录 → 已知限制。一个用例只通过部分 variant 时逐项列出，不能笼统标整例通过。

普通文档校验只证明链接/ID/依赖/示例可解析，不证明以上任何运行能力。设计草案的验证结果与实现验收分开报告。
