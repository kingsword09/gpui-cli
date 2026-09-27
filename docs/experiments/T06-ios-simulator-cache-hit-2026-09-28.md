# T06：iOS simulator BuildKey manifest cache-hit 切片（2026-09-28）

状态：in_progress。本切片仅为非 live iOS simulator build/run 启用本地 manifest cache hit；
真机、live iOS 和 Android 路径保持原有构建行为。

## 行为

- iOS simulator 在创建冻结输入计划后，按 platform/BuildKey 获取既有 OS 文件锁；锁覆盖
  manifest 校验、miss 后的 Cargo/Xcode 构建和 manifest 发布；
- 只有 `read_verified` 验证 iOS platform、BuildKey 和整个 `.app` bundle 文件集合，且
  manifest 声明的根正是当前预期 `.app` 路径时才命中；miss、损坏/不匹配 manifest 或
  bundle 变化均按正常 frozen build 重建；
- simulator 命中时返回已验证 `.app` 路径，跳过 Cargo、XcodeGen 和 `xcodebuild`；`gpui run ios`
  仍为本次调用安装并启动 simulator app，不复用运行身份；
- 真机每次仍重建，但继续持有相同 BuildKey 锁并发布 manifest。当前 BuildKey 没有纳入本机
  signing identity、Provisioning Profile 等外部签名状态，因此不把真机产物作为可复用缓存；
- 命中之前仍会生成并校验冻结输入快照以计算 BuildKey；缓存命中不代表免除源码扫描/复制成本。

## 验证

- iOS manifest fixture 验证完整预期 `.app` 可命中；新增文件导致验证 miss；即使存在另一个
  内容完整且绑定同 key 的输出根，也不能冒充预期 app bundle；
- 本 PR 运行 workspace fmt、clippy、全量测试和设计文档检查；CI 覆盖 Linux/macOS/Windows。
  fixture 验证 manifest/cache 判定逻辑，不冒充真实 Xcode、simulator 或真机安装证据；
- 缓存等待仍不可取消；OS 锁串行化请求，不代表共享一个可订阅/取消的在途任务。

## 未覆盖

- 真机签名输入建模后才能启用 physical-device cache hit；Android cache hit 尚未接入；
- Xcode/SDK/toolchain 版本、未声明 build-script I/O 等输入边界仍未完整纳入 BuildKey，不能
  宣称任意环境下的完整可复现构建；
- live builder、在途任务共享/取消引用、缓存清理与容量预算仍未接入。
