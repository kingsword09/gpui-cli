# S04：desktop check 视觉 baseline 接线（2026-09-28）

状态：in_progress。本切片把静态 baseline 合同接入 desktop check 的真实 observation
artifact；它不创建、批准、更新或放宽任何 baseline。

## 接线

- ScenarioRunner 为 screenshot_matches 提供 baseline-specific resolve seam，保留
  runtime-agnostic executor 的断言语义。
- desktop runner 从 scenario_ready 保存实际 fixture/environment，检查 scenario、fixture
  hash、theme、locale、logical viewport、DPI、scope、target 和可用的 backend/font
  fingerprint。
- 从 control artifact store 读取 PNG 的 ArtifactInfo/有界 chunk，验证 published 状态、
  kind、size、offset/EOF、base64 和 SHA-256；不会读取任意宿主路径。
- 使用 exact-sha256-v1 comparator，结果区分 matched、different 和 not_comparable。缺失、
  损坏、key 不可比和环境字段缺失都保持 inconclusive。
- different 在两张 PNG 能可靠解码且尺寸相同的情况下生成红色差异图，原子写入项目
  .gpui/checks/，报告保存相对路径、字节数、SHA-256、变化像素数和尺寸。diff 写入失败
  只影响诊断引用，不把视觉失败变成通过或 inconclusive。
- 断言 actual 保存 artifact_id、baseline_id、key、capture scope/provider、逻辑/像素
  尺寸、scale、comparable/matches/reason 及可选 diff 引用，便于复核结果所使用的输入。

## 当前限制

生成模板能报告 OS、theme 和 locale；backend 与 font fingerprint 通过显式
GPUI_PREVIEW_BACKEND / GPUI_PREVIEW_FONT_FINGERPRINT 环境值传入。未提供这两个真实
值时，不会把截图 hash 当成严格 baseline 通过。baseline approval 已移到独立的
gpui baseline approve 命令；当前仍不支持 tolerance、mask、动态区域配置、移动端
provider 或矩阵调度。

## 验证

    cargo fmt --all -- --check
    cargo check --workspace --locked
    cargo test --workspace --locked
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo x check-design-docs
    git diff --check

新增纯 Rust 覆盖：

- screenshot assertion 会调用 runner 的 baseline resolve seam；
- PNG artifact metadata 被保留到 assertion report；
- 无 PNG artifact 不伪造 screenshot evidence；
- artifact chunk/hash 校验在 desktop runner 中有界执行。
