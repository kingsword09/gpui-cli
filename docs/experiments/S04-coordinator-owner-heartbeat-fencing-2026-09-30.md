# S04：coordinator owner heartbeat/fencing（2026-09-30）

状态：PR #201 已 squash 合并为 `13fc5c8`。本切片把 coordinator 的持久 owner 记录从一次性
ownership 证据推进为带 fencing token 和 heartbeat 的 leader 状态边界；OS output lock 仍是唯一
允许接管的活跃性依据。

## 实现范围

- coordinator schema 升为 v2，`BuildCoordinatorRecord` 增加 `heartbeat_at_ms` 和每次 attempt
  唯一的 `fencing_token`；旧 schema 记录不参与当前选举，会按既有 stale record 路径重新竞争。
- 每个 leader 在发布 `building` record 后启动后台 heartbeat，默认 10 秒刷新一次；刷新在 coordinator
  state lock 内执行，并要求 attempt、owner、BuildKey、kind、platform 和 fencing token 全部匹配。
- terminal publish 前再次在 state lock 内校验当前 owner。被替换的旧 leader 不能用迟到的 succeeded/failed/
  cancelled/partial 结果覆盖新 attempt，返回明确的 fencing-lost 错误。
- follower 发现 building record 丢失且成功取得 output OS lock 时，按 fencing/owner identity 原子写入
  abandoned marker；超过 30 秒没有 heartbeat 时使用 stale-heartbeat 诊断 marker。heartbeat 过期本身
  不授权强抢，必须先取得 OS lock。
- 普通失败、`Cancelled`、`Partial` 和 superseded 的既有终态/重试语义保持不变；heartbeat 只提供活性
  诊断和迟到发布防护，不把部分输出变成可消费 artifact。

## 验证

- 本地通过 365 个单元测试及全部集成/协议测试、workspace clippy、fmt、design docs 和 diff check。
- 新增回归覆盖当前 fencing 刷新、替换 owner 拒绝 heartbeat、替换 owner 拒绝 terminal publish，以及
  stale heartbeat 不绕过 output OS lock。
- PR 与 push 两套 CI 的 Linux/macOS/Windows、desktop-template、android-template、baseline-driver
  和文档门槛均通过。未发布版本，未创建 tag。

## 未覆盖与下一步

当前 heartbeat 是 coordinator 本地线程，仍不等同于远端/设备租约；它不取代 output OS lock，也没有
跨主机时钟一致性承诺。隐藏 build.rs/Gradle/NDK/Xcode 输入、physical iOS signing、Android custom/
release signing-sensitive preview 和真实设备连续验收仍未完成。下一步补齐签名敏感 BuildKey 输入边界
与移动真实场景证据，再推进剩余 preview/cache orchestration。未发布版本，未创建 tag。
