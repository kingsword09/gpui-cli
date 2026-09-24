# O04：scene completed 证据（2026-09-24）

状态：本子 PR 实现了 runtime 到 supervisor 的 scene 完成事件和窗口状态记录。
它是观察编排的前置证据，尚未实现 observe --sync、scene readback 或真实 backend
present 回调。

## 已实现

- app protocol 新增 scene_completed，携带 window_id、单调 scene_epoch、
  source_revision、asset_revision 和可选 presented_frame_id；
- 模板 runtime 暴露 report_scene_completed，发送状态经过窗口、run、连接和
  revision 绑定；
- WindowRegistry 保存最近一次 scene_epoch、使用的源码/资源 revision、完成时间
  和可选 presented_frame_id；
- supervisor 拒绝未知窗口、已关闭窗口、旧连接、revision 不匹配、epoch 回退和
  重复 epoch；
- required_loaded 仍由 UI adapter 显式报告，scene_completed 不会由后台资源线程
  自动生成；
- presented_frame_id 只有平台 backend 已验证时才应传值，缺少验证时保持 null。

## 证据

- 协议 roundtrip 覆盖 scene_completed 字段；
- WindowRegistry 测试覆盖首次接受、重复 epoch 和旧连接拒绝；
- app channel 测试覆盖当前 run 的 scene_completed 事件，并把 scene_epoch 写入
  windows 查询状态；
- runtime 测试覆盖报告去重和可选 presented frame 字段。

## 边界

scene completed 只证明 GPUI adapter 已完成可用于后续读回的 scene。它不证明 GPU
已经呈现到屏幕；presented_frame_id 目前没有任何平台实现会自动生成。O04 后续
需要把这个事件接入 observe 的 settle/捕获步骤，再由 O05 增加 scene readback 和
语义快照关联。

