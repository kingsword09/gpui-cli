# O04：supervisor build request（2026-09-24）

状态：本子 PR 将显式 build request 接入 live coordinator。control handler
只负责鉴权、登记和入队；实际构建仍由 watcher/键盘循环的单线程编排器消费。

## 已实现

- `gpui dev build` 通过当前 session control socket 提交一次 build request；
- supervisor 以单槽 pending trigger 保存请求，不持有 event store 锁等待构建；
- live coordinator 在等待 watcher 事件时消费请求，在一次构建进行中到达的请求
  会与下一轮重建合并；
- `build.requested` 事件记录 request_id、是否与已有 trigger 合并及队列深度；
- control 与 coordinator 之间没有直接跨线程启动编译，保持 build/run 单飞约束。

## 证据

- control 集成测试确认请求可从鉴权 control endpoint 入队并由 coordinator 取出；
- 两个连续请求共享一个 pending trigger，避免显式请求制造无界队列；
- workspace 测试、clippy、格式和设计文档检查覆盖现有 live 回归。

## 边界

该切片只解决构建触发入口，还没有 operation_id、总 deadline、observe --sync
等待状态、scene capture 或不可变 observation。下一步是把有限 operation 状态机
接入同一 coordinator，再实现按 revision 等待 build/run/scene 的 observe 编排。
