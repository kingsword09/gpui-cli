# F02：package 与 clean-directory 边界（2026-09-22）

状态：in_progress。本子 PR 修复生成 workspace 的发布元数据边界：git 依赖继续
锁定不可变 revision，同时声明对应发布版本，使 Cargo 可以生成 package；CI 再
将 `.crate` 解压到全新目录并重新 `cargo check`。

## 依赖契约

- `gpui-kit` 使用精确 `version = "=0.6.1"` 与固定 `rev`；
- `gpui-mobile` 使用精确 `version = "=0.1.0"` 与固定 `rev`；
- 模板不引用开发者机器绝对路径；
- 依赖版本和 revision 同时写入 generated workspace，供 manifest/lock 审计。

## 可复现验证

desktop-template CI 在 debug/release/feature 边界检查后执行：

    cargo package --workspace --allow-dirty --no-verify
    cargo package --workspace --allow-dirty --no-verify --list

然后把 `.crate` 解压到独立临时目录：app package 直接以 `--locked` 检查，desktop
package 使用仅存在于临时 workspace 的 crates.io patch 指向同目录 app package，
再检查 desktop 依赖闭包。patch 只模拟尚未上传的同一批次 app package，不进入模板
或发布 manifest。

本地此前已复现缺少 git/path 依赖 version 时 Cargo 拒绝 package；本 PR 修复后应由
该 clean-directory job 捕获回归。

## 尚未覆盖

发布 registry 中所有依赖的真实可下载性、完整 protocol/runtime crate 拆分、旧
CLI/v1 与新 runtime 的 C-01/C-03/C-04/C-05 兼容矩阵仍需后续 F02 子 PR；F02
继续保持 `in_progress`。
