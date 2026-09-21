# 研究来源、设计决策与实验关卡

状态：研究记录与拟议决策。调研日期：2026-09-21。代码基线：`6d091b6`。

本文区分外部工具已声明的能力、本项目的实现状态和未来实验。外部资料不是本项目已经集成或验证某能力的证据；下列链接为上一轮调研实际查阅的官方页面/项目仓库。

## 1. 来源索引

| 编号 | 官方来源 | 调研所得事实 | 本项目借鉴/约束 |
| --- | --- | --- | --- |
| REF-01 | [Flutter AI tools](https://docs.flutter.dev/ai/tools) | SDK诊断、符号解析、runtime检查、Skills、package skills共同构成工具体系 | A03绑定版本的知识和组件目录；不只添加MCP端点 |
| REF-02 | [Expo MCP](https://docs.expo.dev/mcp/) | 本地截图/testID/日志与远程构建工具；iOS本地能力有模拟器限制 | M01平台适配与A01能力声明；不假定真机等价 |
| REF-03 | [Compose Hot Reload](https://kotlinlang.org/docs/multiplatform/compose-hot-reload.html) | 桌面热重载；实验性MCP可截图、读取语义树、输入与日志 | 桌面共享UI先形成可用闭环；标注实验与平台边界 |
| REF-04 | [Dioxus 0.7 hot reload](https://dioxuslabs.com/learn/0.7/essentials/ui/hotreload/) | RSX、assets、实验性Rust hotpatch分层；文档说明tip-crate限制 | 保留L0/L1/L0.5；S05只尝试显式数据参数 |
| REF-05 | [Slint AI/MCP](https://slint.dev/blog/slint-and-AI-MCP) | 布局树、输入、截图、headless和解释执行UI热更新 | 查询真实结构；GPUI headless必须自己做PoC |
| REF-06 | [Avalonia DevTools MCP](https://docs.avaloniaui.net/tools/developer-tools/mcp) | 运行应用/预览器、属性/样式/输入；需相应付费许可 | 显式组件预览和属性查询；不复制私有实现 |
| REF-07 | [Storybook MCP](https://storybook.js.org/docs/ai/mcp/overview) | 组件manifest、stories、交互/a11y测试、变化关联、可选MCP Apps | 场景作为文档、预览、测试共同输入；能力仍有框架差异 |
| REF-08 | [SwiftUI/Xcode previews](https://developer.apple.com/documentation/xcode/adding-previews-to-your-interface-files) | 独立视图、样例数据、配置变体、可复用昂贵上下文 | S01/S02提供最小fixture和可重置场景 |
| REF-09 | [Playwright Trace Viewer](https://playwright.dev/docs/trace-viewer) | 动作前后快照、源码、日志和失败产物按时间关联 | M05复现包与不可变观察；不把DOM实现照搬到GPUI |
| REF-10 | [Maestro MCP](https://docs.maestro.dev/get-started/maestro-mcp) | compact hierarchy、截图、Flow执行、设备/云运行和Viewer | 可用作OS输入适配器；GPUI无障碍支持先验证 |
| REF-11 | [Qt/Squish MCP](https://www.qt.io/quality-assurance/blog/from-assistant-to-autonomous-tester-enable-your-ai-agent-to-understand-and-control-your-application) | 技术预览使用运行状态和UI输入，强调大型界面的紧凑表示 | 查询/分页/摘要优先；厂商效率数字不作为本项目承诺 |
| REF-12 | [Tauri capabilities](https://v2.tauri.app/security/capabilities/) | 按窗口/能力声明权限并生成schema | 多客户端按scope校验，接口共享schema |
| REF-13 | [Tracy](https://github.com/wolfpld/tracy) | CPU/GPU、内存、锁与帧关联，存在Rust绑定 | G01/G03可选适配，先测instrumentation开销 |
| REF-14 | [RenderDoc](https://github.com/baldurk/renderdoc) | Vulkan/D3D/OpenGL等帧捕获；不支持Metal | 按API选择provider，不能统一承诺所有GPU backend |
| REF-15 | [Apple Metal debugger](https://developer.apple.com/documentation/xcode/metal-debugger) | GPU trace、资源/pass/shader分析与性能统计 | 捕获作为证据产物，独立于普通截图 |
| REF-16 | [Apple：GPU issues with AI agents](https://developer.apple.com/documentation/xcode/investigating-gpu-issues-with-ai-agents) | gpudebug提供文本可发现接口，可复用已加载trace的分析session | G03按需查询trace；先探测本机工具链 |
| REF-17 | [Codex MCP](https://developers.openai.com/codex/mcp) / [Plugins](https://developers.openai.com/codex/plugins) | 可连接stdio/HTTP MCP，支持的产品表面可组合Skills与MCP | A01/A03使用宿主能力，不内置模型；发布时重核支持矩阵 |
| REF-18 | [MCP Apps](https://modelcontextprotocol.io/extensions/apps/overview) | 可在支持的宿主中显示交互HTML资源；客户端支持不一致 | A04有图片/链接/JSON降级，核心服务不依赖该扩展 |

资料可变。实现某个接口时重新确认所依赖的具体版本/参数；项目本身的协议以本组设计和后续实现测试为准。

## 2. 本地证据

| 编号 | 事实 | 证据/限制 |
| --- | --- | --- |
| LOCAL-01 | D1已有错误/事件查询，尚无UI观察 | [session.rs](../../src/devserver/session.rs) 的能力声明及 [D1记录](../DESIGN-agent-live-feedback.md) |
| LOCAL-02 | Subsecond/GPUI PoC失败 | [Live设计附录C](../DESIGN-live-mode.md) 记录macOS部分链接丢失IOSurface框架；不等于所有未来版本永久不可用 |
| LOCAL-03 | 当前GPUI相关方法存在适用边界 | [Agent设计](../DESIGN-agent-live-feedback.md) 记录a11y激活、导出字段、test-support和帧时机问题；仍须真实窗口PoC |
| LOCAL-04 | 本机Xcode26.2找不到gpudebug | 调研时执行 `xcrun --find gpudebug` 失败；只说明这套本地工具链，不推断其他Xcode版本 |
| LOCAL-05 | PR #18的工程验证通过 | Rust单元/集成、三OS CLI CI、macOS模板check和Android宿主打包；不代表真实移动GPUI运行覆盖 |

## 3. 决策记录

| 决策 | 选择 | 原因 | 重审条件 |
| --- | --- | --- | --- |
| ADR-01 | CLI和MCP共用service层 | 成功判定、身份、幂等和错误必须一致 | 出现无法表达的宿主交互，先扩展适配层 |
| ADR-02 | API/proto v2与v1兼容投影 | 旧事件枚举和archive reader无法识别新kind | 明确停止支持旧版时另做迁移公告 |
| ADR-03 | 观察绑定源码/run/window/scene | 避免拿旧窗口或迟到回复验收新代码 | 不接受削弱该约束的优化 |
| ADR-04 | 场景显式fixture/reset | 控制导航、网络、随机和历史状态 | 证明更快reset同样清理全部异步状态后启用优化 |
| ADR-05 | 不将Rust热补丁列为近期依赖 | 本地PoC未过，移动入口与依赖布局还有问题 | 新版本通过同一GPUI夹具，且跨平台/状态边界有证据 |
| ADR-06 | 先本地runner，再远程 | 能力和失败模型先验证，避免前置云系统 | G3本地矩阵稳定且实际有远程需求 |
| ADR-07 | 默认按需截图、分页树和摘要 | 大型UI不能无限占据Agent上下文和网络 | 基准证明更高频传输有明确收益且在配额内 |
| ADR-08 | SDK/模板/文档绑定版本 | 减少复制旧runtime和使用错误API | 上游提供稳定独立观察协议后收缩本地适配 |
| ADR-09 | profile与生产release分开 | 采样、捕获与控制影响性能和行为 | 仅针对被测开销足够小的指标调整默认策略 |
| ADR-10 | 保留主动输入核验 | watcher/mtime不构成完整版本证明 | 新索引在对抗用例中证明无漏报后替代部分扫描 |
| ADR-11 | 热参数只作用于显式注册项 | 可获得快速视觉迭代，成本和语义可控 | S05基准/源码固化验证通过再扩展 |
| ADR-12 | 基线更新独立于修复 | 防止通过接受错误结果消除失败 | 不接受自动放宽断言/扩大mask的“自修复” |

## 4. PoC 关卡与失败后的动作

### P01：macOS观察能力

输入：固定GPUI0.3.5及当前依赖revision，counter/form/list真实窗口。

实验：验证图像读回、scene完成语义、UI探测、无需屏幕阅读器常开的语义树、节点ID/bounds导出、窗口关闭/设备丢失时行为。记录命令、环境、截图/树、耗时、依赖feature和源码修改。

通过：至少一条可信截图路径和UI心跳可用，能够给出捕获版本与一致性等级；语义树缺失的具体接口已定位，有可实现的最小上游/adapter方案。

失败处理：scene readback不可用时可以评估OS window capture，记录best_effort限制；不把它提升为same_scene。若没有任何可验证截图路径，G1保持阻塞。树失败不阻止只有截图的G1，但G2的稳定选择器/语义断言必须等待解决。

### S05：热参数

输入：至少20次颜色/间距/文字等注册参数修改，包含撤销、重启和源码并发修改。

通过：预览正确、overlay版本可见、固化patch可审阅且不覆盖并发编辑，测量显示相对普通重建有收益。

失败处理：保留普通Live重建和场景reset，撤回实验capability；不引入通用解释器兜底。

### G03：GPU工具

输入：一个可复现渲染错误、一个已知GPU负载退化和一个正常对照。

通过：能自动取得属于正确run/帧的trace，分析结果引用具体命令/资源，工具进程可回收。每个backend独立记录结果。

失败处理：返回unavailable和建议的人工捕获路径；不影响普通观察/场景验证。gpudebug缺失时不自动安装或切换Xcode。

## 5. 实验记录模板

每份记录存入拟议 `docs/experiments/<task-id>-<date>.md`，完成实验时才创建；内容至少有：

1. 假设与明确的通过/失败判据。
2. 代码commit、依赖revision、硬件/OS/SDK/工具版本。
3. 原样可重跑命令、受控输入和必要配置。
4. 实际输出、截图/树/trace引用和hash。
5. 未达到的判据、适用平台、性能与资源代价。
6. 决策：adopt / limited_adopt / reject / retry_after_upstream_change。
7. 后续任务、负责模块和重试触发条件。

不得把“看到了一个截图”“API编译成功”“模型说能支持”写成协议/跨平台能力已通过。
