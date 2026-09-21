# 开发文档导航

当前功能以项目 [README](../README.md) 和代码为准；设计草案不代表已实现。

## 后续路线与可执行计划

- [Agent-native 跨平台开发总路线](ROADMAP-agent-native-development.md)：目标、现状、架构、G0–G4 门槛。
- [35 项实施工作包](roadmap/implementation-backlog.md)：依赖、代码落点、PR 拆分、验收与回退。
- [68 项验收与实验操作单](roadmap/acceptance-matrix.md)：故障注入、证据、平台/CI 层级及12项 Agent 基准。
- [研究和设计决策](roadmap/research-and-decisions.md)：跨平台工具的官方来源、取舍和上游实验关卡。
- [可解析配置/响应草案](examples/README.md)：场景、矩阵、性能预算、观察、复现包、模板与 doctor。

## 专项设计

| 主题 | 设计 |
| --- | --- |
| 可信反馈 | [观察协议](design/observation-protocol.md) |
| 组件开发与测试 | [场景、输入与检查](design/scenarios-and-checks.md) |
| 跨端执行 | [Runner、租约、矩阵与复现](design/platform-matrix-and-repro.md) |
| 开发/运行效率 | [性能预算与 GPU 捕获](design/performance-and-gpu.md) |
| Agent 工作流 | [CLI/MCP、版本知识与报告](design/agent-interfaces.md) |
| 工程基础 | [工具链、runtime、模板升级与缓存](design/toolchain-and-upgrades.md) |

## 既有设计与实现记录

- [Live 模式](DESIGN-live-mode.md)：重建、资源、状态恢复与热补丁实验。
- [D1 Agent 反馈](DESIGN-agent-live-feedback.md)：当前 status/diagnostics/events 的背景及实现记录。
- [设备选择与生命周期](DESIGN-devices.md)：设备发现、稳定选择和跨平台约束。

文档修改后按 [贡献指南](../CONTRIBUTING.md) 运行一致性检查；平台能力晋升需另行执行真实验收。
