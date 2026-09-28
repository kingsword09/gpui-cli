# S04：视觉 baseline 静态契约与精确比较（2026-09-28）

状态：`in_progress`。本切片固定 baseline 的安全读取、可比性和 exact hash 语义；它不自动
批准/更新基线，也不把缺失 baseline 转成通过。

## 文件契约

baseline 放在项目内：

```text
dev/baselines/<target>/<baseline_id>/manifest.json
dev/baselines/<target>/<baseline_id>/image.png
```

manifest schema-v1 使用 deny-unknown-fields，包含完整 `BaselineKey`：scenario、fixture
hash、target/backend/OS、实际 viewport、scale、theme、locale、font fingerprint 和 capture
scope；图片声明相对路径、字节数、PNG 像素尺寸和 `sha256:`。算法目前必须是
`exact-sha256-v1`、version=1、tolerance=0。

## 行为

- baseline id/target 只能是安全单段标识；manifest/image 不允许逃出 baseline 目录，也不跟随
  symlink；manifest 最大 1MiB，PNG 最大 64MiB。
- 读取先校验 schema、算法、路径、文件大小、PNG signature/IHDR 尺寸和 SHA-256，再进行
  BaselineKey 比较；路径/内容损坏是 invalid，不是 not comparable。
- 缺 manifest 返回 `Missing`；key 任一字段（含 viewport、DPI、locale、scope）不符返回
  `NotComparable` 并列出字段；不会缩放、猜测字体或跨平台复用。
- exact comparator 先比较 PNG 尺寸，再比较 hash，输出 matched/different/not_comparable；
  不写 diff、不更新 manifest、不扩大 mask。

## 验证与边界

- 5 个纯 Rust 测试覆盖缺失、unknown manifest field、hash 篡改、key mismatch、精确匹配/差异
  和尺寸不可比。
- `gpui check` desktop runner 已接入这个 loader/comparator：`screenshot_matches` 绑定当前
  observation 的 PNG artifact，按 chunk 读取并校验 artifact manifest/hash，再把
  `baseline_id`、完整可用的 `BaselineKey`、artifact_id、尺寸/scale/scope/provider 和
  matched/different/not_comparable 写入断言 `actual`。artifact 读取失败不会复用旧图片。
- strict key 需要 runtime 的实际 backend 和 font fingerprint；生成模板会报告 OS，并预留
  `GPUI_PREVIEW_BACKEND` / `GPUI_PREVIEW_FONT_FINGERPRINT` 作为显式 runtime 环境值。缺失
  这些字段时 check 返回 `inconclusive`，错误不会猜成通过。
- 当前仍不生成 diff artifact；本切片的 loader/comparator 是无 GPU 的稳定 seam，实际
  macOS Screen Recording/semantics 权限和真实基线批准仍需平台验收。
- 容差、动态区域 mask、baseline approval/review、跨平台 backend 目录和独立更新命令属于后续
  S04/R-05 切片。
