# T06：iOS preview verified cache hit（2026-09-29）

状态：`in_progress`。PR #171 已 squash 合并为 `f6801e9`。本切片把已有 preview output lock
和 verified artifact manifest 语义接入 iOS simulator live preview；不把真机签名产物标为可复用
缓存。

## 行为

- live preview 从受控环境读取 `GPUI_PREVIEW_BUILD_KEY_HASH`、target-specific output root 和
  iOS DerivedData 路径，并在同一 output root 上取得 `BuildOutputLock`；锁覆盖 manifest 校验、
  miss 后的构建和 manifest 发布；
- 只有 simulator 路径尝试命中独立的 `preview-artifact-manifest.json`。manifest 必须绑定 iOS
  platform 和当前 BuildKey，逐文件验证完整 `.app` bundle 的路径、大小和内容 hash，且声明的
  root 必须正是当前 scheme 的 `Debug-iphonesimulator/<scheme>.app`；
- 命中时直接返回已验证 `.app`，跳过 `rustup target add`、Cargo、XcodeGen 和 `xcodebuild`；
  live loop 随后仍按本次 run 安装/启动 simulator，不复用设备运行身份；
- simulator 或 physical live build 成功后都会发布对应 manifest，但 physical 路径不会读取它来
  命中。真机继续每次重建，因为 signing identity、Provisioning Profile 等本机输入尚未进入
  BuildKey；
- manifest 缺失、platform/key 不匹配、bundle 缺失、根路径不匹配、文件新增/删除或内容变化
  都按 cache miss 回到正常构建。命中之前仍需完成已有的输入扫描/BuildKey 计算。

## 验证

- 新增 fixture 单测确认完整预期 simulator `.app` 根可匹配，另一个 bundle 根不能冒充命中；
- 本 PR 本地通过 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked
  -- -D warnings`、`cargo test --workspace --locked`（336 个单元测试及全部集成/协议测试）、
  `cargo x check-design-docs` 和 `git diff --check`；
- PR #171 的三 OS check、desktop-template、android-template、baseline-driver required checks
  全部通过；没有发布或 tag。测试验证 manifest/cache 判定逻辑，不冒充真实 Xcode、simulator 或
  真机安装验收。

## 未覆盖

- physical-device cache hit 仍需先建模签名 identity、Provisioning Profile 和相关外部输入；
- Android preview verified cache hit、跨独立命令构建 ownership、同 key 在途任务 coalescing/
  取消引用、增量索引、预热和性能预算仍未接入；
- XcodeGen、任意 build-script 隐藏 I/O 等输入边界仍不完整，不能据此宣称任意环境下完全可复现。
