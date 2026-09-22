# F02：生成模板 feature/release 边界第一子 PR（2026-09-22）

状态：in_progress。本子 PR 先建立 `gpui-dev` / `gpui-profile` 的显式 feature
边界，并让 `gpui run --live` 在 debug 构建中明确传递 `gpui-dev`；协议/runtime
抽取和独立发布 crate 仍未宣称完成。

## 边界契约

- 普通 debug/release 默认不启用开发控制入口；debug 下 asset source 为空；
- `gpui-dev` 只允许 debug 构建，并由 live CLI 的 Cargo build 显式开启；
- release + `gpui-dev` 编译失败；
- `gpui-dev` + `gpui-profile` 编译失败；
- `gpui-profile` 可独立作为受限 profile 占位 feature，尚未提供完整采样 API；
- desktop feature 通过 app crate 转发，iOS/Android live Rust build 直接传递
  `--features gpui-dev`。

## 可复现验证

CI desktop-template 对生成项目执行：

    cargo check --workspace
    cargo check --workspace --features gpui-dev
    cargo check --workspace --release
    cargo check --workspace --release --features gpui-dev   # expected failure
    cargo check --workspace --features gpui-dev,gpui-profile # expected failure

仓库本地还通过了模板单测、完整 workspace test/Clippy、design-docs 检查；真实
generated project 的 debug、gpui-dev、release 和两个预期失败组合已在本机验证。

## 尚未覆盖

`gpui-dev-protocol` / `gpui-dev-runtime` 独立 crate、发布包依赖审计、profile 的
实际受限测量能力和 C-02 的完整 package/clean-directory 证据仍需后续 F02/T04
子 PR；F02 与 T04 都继续保持 `in_progress`。
