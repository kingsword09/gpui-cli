# M04：iOS 构建产物 manifest 发布切片（2026-09-28）

状态：`in_progress`。本切片把 artifact manifest 接入 iOS 非 live `build`/`run` 的成功路径；
不启用缓存命中，也不改变 live iOS。

## 行为

- Xcode 成功且 BuildKey 私有 DerivedData 中存在预期 `.app` 目录后，CLI 扫描完整 app bundle，
  并原子发布 `.gpui/builds/ios/<key>/artifact-manifest.json`；
- manifest 绑定 iOS platform/BuildKey，递归记录 bundle 内每个普通文件的相对路径、大小、
  SHA-256 和 executable 标志；缺失、变化、新增文件或 symlink 会使 `read_verified` 失败；
- manifest 发布失败会让本次 iOS build/run 返回错误，不会把未校验的 app bundle 报告为已完成；
- iOS 安装/启动身份仍在 build 后新建，manifest 不改变 run 语义。

## 验证

- 命令单元测试用临时 iOS BuildKey layout 构造 `.app` bundle，验证 manifest 与 platform/key
  完整绑定；加入未登记资源后验证失败；
- workspace fmt、clippy、全量测试、设计文档检查和 GitHub 平台 CI 为 PR 门槛；测试 fixture
  不冒充真实 Xcode/simulator 构建证据。

## 未覆盖

- desktop build 命令尚未发布 artifact manifest；live iOS 尚未接入；
- bundle 内 symlink 当前明确拒绝，若未来模板引入带 symlink 的嵌入式 framework，需要增加
  能验证相对目标且不越出 bundle 的 symlink 契约；
- 持久缓存、同 key 在途任务合并和 Xcode 未声明隐藏输入仍未接入。
