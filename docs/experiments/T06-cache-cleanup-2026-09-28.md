# T06：BuildKey cache cleanup 切片（2026-09-28）

状态：in_progress。本切片提供显式 `gpui cache clean --max-bytes <N>`，按当前项目
`.gpui/builds` 下 desktop/Android/iOS 的 BuildKey 输出做有界本地清理。

## 行为

- 只扫描平台目录下名字为 64 位小写 SHA-256 的 key root；按树中最新 mtime 的最旧 key
  优先回收，直到识别到的内容不超过预算；只处理受控的三种平台目录，不遍历其他用户路径；
- 每个 key 清理前使用同一个 `.build-output.lock` 做非阻塞尝试；活动构建/读取会跳过，不等待
  也不删除；清理保留 key root 和锁文件，删除其余普通文件并回收空目录；锁释放后后续 build
  会重新 `prepare` 自己的 staging；
- dry-run 不创建锁、不删除文件，但报告预计可回收字节；包含 symlink 或特殊文件的 key 标记
  unsafe 并跳过，避免跟随链接或误删不受控内容；
- 清理是显式命令，未自动接入每次 build，也不假设同 key 在途任务有 subscriber/refcount。

## 验证

- 单测覆盖预算回收、dry-run 不改动、活动锁跳过、symlink key 内容跳过、symlink builds root
  拒绝，以及清理后保留锁并允许 layout 重新创建 staging；
- CLI 暴露 `gpui cache clean --max-bytes <N> [--dry-run]`；本切片未把用户项目外的目录加入
  清理范围，也没有删除任何真实用户缓存。

## 未覆盖

- 不提供按项目/平台/时间的复杂过滤、跨项目全局 cache index、远程缓存、自动后台回收或
  subscriber/refcount；活动 key 可能导致最终 remaining bytes 高于预算，命令会报告跳过原因；
- 清理只按文件树大小与 mtime 判断，不重验证 artifact manifest，也不修复已损坏缓存；下一次
  build 的既有 manifest 校验仍决定是否命中。
