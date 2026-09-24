# O02：required-loaded 资源 ACK（2026-09-24）

状态：in_review。本子 PR 将“应用声明必需资源已加载”从传输到达和缓存失效中独立出来。

## 实现

- runtime 提供 `require_asset(path)` 与 `clear_required_assets()`，由应用声明当前场景的
  必需资源；
- desktop/Android 的 `DevAssetSource::load` 对实际读取的 bytes 校验当前 declared hash，
  成功记录 loaded，缺失或 hash mismatch 记录 failed；
- UI adapter 在确认 scene 完成后调用 `report_required_assets_loaded(window_id, scene_epoch)`；
  runtime 以 `assets_required_loaded` 携带 `required`、`loaded`、`failed`、scene 元数据和当前
  `transfer_id`/`asset_revision` 回报，状态变化去重；
- supervisor 记录 `assets.required_loaded`，并按当前 transfer identity fencing，不能改写
  `assets_confirmed`。

## 边界

iOS 的 `Resource::Path` 读取尚未接入同等的 decode/load 回调；scene_epoch 是 UI adapter
提供的完成标记，仍不等同于 GPU presented frame，不能单独证明屏幕已经呈现。
