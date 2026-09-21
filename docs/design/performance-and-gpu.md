# 设计契约：开发延迟、场景性能与 GPU 证据

状态：拟议，未实现。任务：F01、G01–G03、Q02。依赖：[场景](scenarios-and-checks.md)、[平台矩阵](platform-matrix-and-repro.md)。

## 1. 三类性能问题分别测量

| 问题 | 测量对象 | 使用的构建 |
| --- | --- | --- |
| 开发循环慢 | 扫描、编译、链接、打包、安装、启动、观察 | 正常 debug Live |
| 应用运行慢 | 布局、paint、帧间隔、资源加载、内存 | 固定配置的 profiling 构建 |
| GPU 图形错误/瓶颈 | render pass、资源、shader、GPU counters | backend 支持的捕获配置 |

不要把这三类结果混在同一 FPS 指标里。截图正确不能证明 GPU 工作正确或性能正常；GPU capture 开启后的时序也不能直接用作低开销性能基线。

## 2. 开发循环追踪 F01

在 supervisor 统一单调时钟上记录下列 span：

`inputs.scan`、`build.queue`、`cargo.compile`、`native.package`、`device.lease_wait`、`device.install`、`app.launch`、`app.connect`、`ui.ready`、`assets.apply`、`observation.capture`、`artifact.publish`。

一个 span 包含 trace_id、span_id、parent_id、session/build/run/operation ID、起止单调时间、结果和必要维度。收到外部进程事件的时间与事件在设备实际发生的时间分开保存；不比较未同步的机器时钟。

对于 Cargo 内部更细的编译瓶颈，优先关联 Cargo 已有 timing 输出；不能通过 CLI stdout 行间隔假装获得逐 crate CPU 时间。外部工具没有细粒度信息时只记录整阶段。

失败、取消、superseded 和缓存未命中都进入统计。报告同时给出样本数、P50/P95、总耗时和阶段构成。性能数据不包含完整命令环境或 token。

## 3. 应用指标 G01

| 指标 | 来源 | 单位/解释 |
| --- | --- | --- |
| `ui.layout_ms` | GPUI 可验证的布局阶段 hook | 每个 scene 的 CPU 耗时 |
| `ui.paint_cpu_ms` | paint/scene 构建 hook | 不包含 GPU 执行时间 |
| `frame.interval_ms` | 可确认的帧边界 | 两帧间隔，不等同于 GPU 时间 |
| `frame.present_latency_ms` | backend 提供的提交/呈现通知 | 能力缺失则 unavailable |
| `gpu.duration_ms` | backend timestamp/counter | 必须校验 query 支持和 disjoint/无效样本 |
| `asset.load_ms` | 应用资源加载器 | 路径以项目内标识输出 |
| `process.rss_bytes` | OS 进程采样 | 与 GPU 显存分开；不同平台定义记录清楚 |
| `gpu.memory_bytes` | backend/平台统计 | 估计或精确值要标注 |
| `ui.invalidations` | 窗口失效/重绘计数 | 用于判断不必要刷新，不单独判定故障 |

第一实现只采集有明确 hook 的指标。需要修改 GPUI 时先提交最小上游适配提案；不能用名字相似的方法估计数据后当成精确指标。

### 3.1 采样和开销

- 默认关闭逐帧全量 trace，仅保留低开销的阶段摘要。
- 显式 profile 时采集；聚合后每秒发布一次，帧原始样本写受限产物。
- 采样队列有上限，丢弃量可见；缺样本的 run 标记 incomplete。
- 每项指标标注 source、采样频率、单位、计数/直方图类型和 scope。
- 用相同场景比较 instrumentation 开/关。初始预算是 CPU 帧耗时 P95 增量不超过 5%；这是实验门槛，超过则降低采样或关闭该指标，不能隐瞒开销。

## 4. Profiling 构建与环境指纹

拟议应用 profile `gpui-profile` 从 release 继承优化配置，保留必要符号；通过独立 `gpui-profile` feature 开启最小指标/场景支持。

默认 release 不启用交互调试端口。profiling 构建即使优化级别接近 release，也不能标成生产二进制；报告记录所有 features、编译参数和 instrumentation 状态。

环境指纹至少包含：OS、CPU/GPU 型号、GPU driver/backend、屏幕/DPI/刷新率、电源模式、构建键、Rust/GPUI版本、场景/fixture hash、字体、locale、采样配置。

完整指纹用于溯源，不直接作为相等比较键。`PerfComparisonKey` 包含硬件/驱动/OS、profile/features/编译器参数、GPUI依赖、instrumentation、场景/fixture/测量step、输入节奏、屏幕/字体/locale/电源条件；排除预期会改变的 source hash、build/run ID。baseline/candidate 的完整 BuildKey 各自保留。只有业务源码不同才是默认允许的变量；换依赖/设备/采样策略需新基线，不因源码 hash 本来不同就拒绝一切回归比较。

电池省电、热降频、后台负载不可控时标记 `environment_unstable`。不支持读取的属性为 unknown；不能默认为“正常”。

## 5. 性能预算与统计 G02

完整例子：[performance-budget.toml](../examples/performance-budget.toml)。拟议命令：

```bash
gpui perf run --scenario list-scroll --target desktop --budget gpui.perf.toml --json
gpui perf compare --baseline perf-10 --candidate perf-11 --json
```

执行步骤：校验预算 → 校验 PerfComparisonKey → 获取独占设备 → 新运行/reset → 预热 → 重复采集 → 检查样本完整性 → 统计 → 比较 → 保存报告。

默认预热 10 次、测量 30 次；测量次数指相同场景工作负载的独立重复。每个重复记录帧样本数、P95 和内存峰值，禁止将某个较短 run 的帧简单拼接后让其权重失真。

预算的 measure_steps 指明原场景中的动作步骤，测量只覆盖这些 step 的实际执行窗口；ready/断言/截图等步骤仍执行，但其成本不混入业务工作负载。示例使用2s分段滚动而非一次瞬时 scroll。每个指标必须声明 min_samples（示例30）；帧不足为 incomplete/inconclusive，不能补零、强制无关重绘或跨 run 拼帧凑样本。

第一版比较算法固定为 `paired-bootstrap-v1`：

1. 每个 run 计算配置的指标统计量，例如该 run 的 frame P95。
2. 对所有 run 统计量取中位数作为 aggregate，保留完整分布。
3. 只启用绝对预算时至少 30 个有效候选 run，不要求历史基线。相对比较时，基线/候选在同一 runner 交替执行至少 30 对，并保存 pair_id；两个不同日期的未配对结果只能显示趋势，不能伪称配对实验。
4. 用固定 seed、2000 次 bootstrap 计算 95% 百分位区间；样本内P95和区间端点统一使用线性插值分位数（Type 7）。绝对预算对候选 run 重采样；相对比较对成对 run 索引重采样，每次重新计算两个中位数、差值和比例。固定随机数实现/版本并记入报告，避免同seed跨实现产生不可解释差异。
5. 绝对规则：候选 aggregate 的区间下界大于 limit 才失败，上界不大于 limit 才通过，跨越 limit 为 inconclusive。
6. 相对规则（第一版仅支持越低越好指标）：基线中位数大于 0；候选/基线的退化百分比超过 max_regression_pct，候选减基线达到 min_delta，且差值 95% 区间下界大于 0，才判 regression。为避免把不确定结果当通过，只有差值区间上界小于 min_delta 或退化百分比区间上界不超过 max_regression_pct 才判通过；其他情况为 inconclusive。
7. 无效/不足样本、环境不可比、区间所需的基线为零时，对受影响规则返回 inconclusive / not_comparable，不回填 0。禁止挑最快一次作为最终结果。

预算分别声明 absolute/relative.enabled；任一启用规则失败则整体 failed，所有启用规则通过才 passed，否则 inconclusive。基线为零时仍可以独立评价绝对规则，但不能静默删除已启用的相对规则。规则边界、单位转换和百分位插值固定在算法版本中。

预算 schema v1 的 `sampling` 指定 warmup_runs、measurement_runs、bootstrap_resamples、confidence、seed；`budgets[]` 指定 scenario、measure_steps、min_samples、metric、unit、per_run（p95/peak）、aggregate（首版 median）、direction（首版lower_is_better）和两类规则。MVP 的 confidence 固定 0.95，少于 30 个有效 run 或 30 对时不出正式结论。perf compare 必须核验 pair_id；没有配对证据时建议重新执行 paired run。

同一场景的视觉/行为检查必须通过，才把性能优化记为通过；更快但删掉了必要渲染的实现不能算优化成功。

## 6. GPU 捕获适配 G03

### 6.1 能力探测

| 适配器 | 目标 | 接入前检查 |
| --- | --- | --- |
| Tracy | CPU/GPU timeline 和帧关联 | Rust绑定、当前backend、可用query和连接方式 |
| RenderDoc | 支持的 Vulkan/D3D/OpenGL 路径 | 安装版本、当前API、可控进程、捕获权限 |
| Metal/Xcode | Metal workload 与 `.gputrace` | Xcode版本、capture支持、签名/权限、可用分析工具 |
| `gpudebug` | 文本化 Metal trace 探索 | `xcrun --find gpudebug`、版本和命令能力 |

RenderDoc 不支持 Metal。2026-09-21 的本机实验为 Xcode 26.2，未找到 gpudebug；Apple 官方文档已介绍 Agent 使用它分析 trace，但本项目尚未完成接入验证。

### 6.2 捕获操作

拟议命令 `gpui perf capture --scenario list-scroll --provider metal --frames 3 --json`。

捕获由显式请求触发，绑定 scenario/run/window/scene 区间。执行器先确认环境可用，再在已知场景步骤附近触发捕获；不能在无法确定目标 app 的情况下捕获整个系统。

结果保存 provider/version、开始/结束帧、输入版本、捕获限制和 artifact_id。GPU trace 默认每个不超过 512 MiB、每 session 最多 2 个；超出提前停止或返回 quota_exceeded，不阻塞 Live 主循环。

### 6.3 分析会话

外部分析工具启动和加载 trace 成本可能较高。同一个捕获文件在有界分析会话中复用，不对每次 query 重新加载。分析会话最多 2 个，空闲 5 min 后释放；客户端断开不无限保留工具进程。

Agent 默认获得摘要、资源/命令目录和可查询句柄，需要时再读具体 pass、shader 或纹理。每个推断必须引用实际 trace 节点和工具输出；“根因”与“待验证假设”使用不同字段。

该能力不是自动修改 shader 的许可。修改仍通过正常代码工作流，并用同一场景重新验证正确性与性能。

## 7. 性能产物和报告

报告包含 raw samples、统计算法版本、baseline/candidate键、区间与预算规则、失败指标、相关截图/日志/trace引用以及不能比较的原因。

普通观察只返回 perf summary，不夹带几千帧原始数据。GPU trace 不通过普通 MCP 图像结果上传；由显式 artifact 读取和本地分析适配器消费。

性能基线的批准和更新独立于代码修复。CI 不能因为本次失败自动更新预算或基线。固定硬件任务与共享 GitHub runner 结果分开：后者可做趋势提示，默认不作为精确 GPU 性能门禁。

## 8. 验收与落点

- F01：supervisor span、汇总命令/产物，P-01/P-02。
- G01：`src/perf/metrics.rs` 与 runtime/backend hooks，P-03/P-04。
- G02：`src/perf/statistics.rs`、budget解析与报告，P-05/P-06。
- G03：`src/perf/providers/` 和分析会话管理，P-07/P-08。
- Q02：固定 Agent 开发任务的速度/正确性评估，A-07/A-08。

见 [验收矩阵](../roadmap/acceptance-matrix.md)。统计单元测试应使用已知分布、明显退化、零基线、样本缺失和不可比环境；真实 GPU 验收必须保存环境和捕获文件，不能仅验证适配器命令字符串。
