# F02：模板窗口 runtime 与 UI probe 泵（2026-09-23）

状态：in_progress。本子 PR 把当前未发布的 app-channel proto 2 接入生成模板：
生成窗口在创建后注册、退出前关闭；网络线程只接收 `probe_ui` 并入队，GPUI
前台任务消费队列、确认窗口仍存在后回传 `ui_probe_result`。

## 实现

- `templates/app/src/live.rs` 增加窗口注册/关闭、UI probe 请求队列和有界的连接前控制消息队列；
- `templates/app/src/lib.rs::pump_live_assets` 同时处理资源失效和 UI probe，UI 状态访问保持在 GPUI 前台上下文；
- desktop、iOS、Android 生成入口统一注册 `main` 窗口，并在 app quit 回调发送关闭事件；
- release 构建保留同一生成入口 API，但 runtime 调用为空操作；
- 连接建立后重放连接前登记的窗口控制消息；
- 模板单测覆盖 JSON dispatch、连接前登记和 probe 入队。

## 验证

在临时生成的 macOS 项目中执行：

    cargo check --workspace
    cargo check --locked --release
    cargo check --locked --features gpui-dev
    cargo test --locked --features gpui-dev

结果：全部通过。当前仍未执行 crates.io 发布；protocol/runtime 继续以内嵌模板和
workspace path 依赖开发。

## 边界

本次只完成 runtime transport adapter，不宣称 UI probe 已等价于 GPU 呈现或业务场景
完成；窗口尺寸/DPI 仍由生成入口的初始元数据提供，后续 backend 适配负责真实更新。
