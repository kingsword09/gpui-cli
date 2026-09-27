# M04：desktop 构建产物 manifest 发布切片（2026-09-28）

状态：in_progress。本切片把 artifact manifest 接入 desktop 非 live build/run 成功构建路径；
live 模式和缓存命中不在范围内。

## 行为

- desktop Cargo build 使用 --message-format=json-render-diagnostics，从 compiler-artifact
  消息读取 Cargo 实际选择的 package binary executable 路径；不按 host OS、target triple 或
  .exe 后缀推测输出位置；
- 仅收集 desktop package 的 bin target，不收录依赖的 build script、库、测试或 registry
  package executable；无 bin artifact 或 executable 位于 BuildKey 输出根之外时失败；
- 构建成功后将这些实际 executable 文件写入
  .gpui/builds/desktop/<key>/artifact-manifest.json，manifest 绑定 desktop platform 和
  BuildKey，并记录每个文件的路径、大小、SHA-256 与 executable 元数据；
- gpui run desktop 在 manifest 发布后仍调用 cargo run -p ...，让 Cargo 保留其默认 binary
  选择与运行时环境行为；两次 Cargo 调用复用同一个冻结 snapshot 和 key 专属 target
  目录，第二次为增量校验/启动。

## 验证

- 单元测试覆盖 Cargo JSON 中 binary/package 过滤、依赖或 library artifact 排除，以及 desktop
  executable manifest 的 BuildKey 绑定和内容篡改拒绝；
- workspace fmt、clippy、全量测试、设计文档检查和 GitHub 平台 CI 为 PR 门槛；生成的
  Cargo JSON fixture 不冒充真实桌面 GUI 启动证据。

## 未覆盖

- 只登记 Cargo 明确报告的 package bin executable，不递归登记任意旁置运行时资源/DLL；
- desktop live builder、缓存命中、同 key 在途任务合并和构建清理尚未接入；
- build.rs/链接器未声明读取的外部文件仍不在冻结输入闭包内。
