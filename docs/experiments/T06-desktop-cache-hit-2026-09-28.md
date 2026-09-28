# T06：desktop BuildKey manifest cache-hit 切片（2026-09-28）

状态：in_progress。本切片在 desktop 非 live build/run 路径上接入本地 manifest cache hit 和
同 key 请求串行化；不宣称已实现可取消的同一在途任务共享。

## 行为

- `.gpui/builds/desktop/<key>/.build-output.lock` 使用标准库 OS 文件锁；锁文件保留在输出根，
  进程退出/崩溃时 OS 自动释放锁；
- desktop `build/run` 在锁内先调用 `read_verified` 校验 schema、platform、BuildKey 和全部
  Cargo executable 输出；`gpui build desktop` 完整命中时跳过 Cargo build，缺 manifest、manifest
  损坏或产物不匹配都作为 cache miss 并执行正常 frozen build；
- build 期间一直持锁到 artifact manifest 原子发布完成，因此并发同 key CLI 请求会等待，之后
  验证并复用刚完成的产物；不同 BuildKey 使用不同锁文件，不互相阻塞；
- `gpui run desktop` 命中时仍调用 Cargo run 启动现有 binary，不复用任何进程/run 身份；Cargo
  仍执行自己的 freshness check，若 Cargo fingerprint 判定缺失/过期可再次编译；因此该路径
  不声称无条件跳过 Cargo 的所有检查；
- 当前 miss 原因简要打印，持久缓存清理/容量预算尚未启用，未验证目录不会被当成命中。

## 验证

- runner cache 单元测试覆盖缺失/篡改 manifest 转 miss、完整 manifest 转 hit、同 key 文件锁互斥
  与释放后第二请求观察到第一个请求发布的完整 manifest；
- 本 PR 运行 workspace fmt、clippy、全量测试和设计文档检查；PR CI 覆盖 Linux/macOS/Windows；
- 测试验证 OS 文件锁的协调和 manifest hit 路径，不将线程 fixture 扩大声称为真实多进程性能
  或取消传播证据。

## 未覆盖

- iOS/Android cache hit、跨平台/远程共享缓存和 artifact 清理尚未接入；
- 等锁请求当前不可取消、无等待 deadline；真正的并发任务 subscriber/refcount/cancellation 未接入；
- desktop bin 以外的旁置 DLL/资源和 build.rs 隐藏 I/O 不在 manifest 闭包内；
- 包含本地 `build.rs` 的 workspace 现在保守 bypass artifact cache reuse，避免将未建模读取集
  当作可命中的完整 BuildKey；记录见
  [T06 build-script cache bypass](T06-build-script-cache-bypass-2026-09-28.md)；
- BuildKey 环境/toolchain 维度的进一步完备化仍需单独证据切片，故不宣称任意项目构建均可
  完全复现。
