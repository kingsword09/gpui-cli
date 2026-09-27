# M04：外部 path package 冻结与重定位切片（2026-09-27）

状态：`in_progress`。本切片将 `CargoInputScope` 中的外部本地 package 复制到冻结副本，
并只在副本内重写 Cargo manifest 的 `path` 字段；不会修改源工作区。

## 行为

- `Inputs::freeze_to_with_cargo_scope` 先对 workspace 和每个外部 root 做有界稳定扫描；
- 外部 root 按 metadata 顺序映射到 `external/0000`、`external/0001` 等受控目录，拒绝
  workspace 内部、workspace 祖先、重叠 root、目录 symlink 和未跟踪目录链接；
- workspace 与外部 package 的 `Cargo.toml` 中，解析到已知外部 root 的 `path` 字段在快照
  内重写为相对 relocated path；源 manifest 保持原字节；
- `FrozenInputs` 保存 workspace/外部 manifest、relocation 证据和合并 content hash；
  hash 不包含机器相关的 source absolute path，只包含快照逻辑路径和内容 manifest。

## 验证

- 外部 package 文件被复制到冻结目录；
- app manifest 的外部依赖从原始相对路径重写为 `../external/0000`；
- 源 app manifest 未被修改；
- 外部 root 越界、重叠、symlink 和不稳定输入路径继续拒绝；
- workspace 全量测试与设计文档检查通过。

## 未覆盖

本切片本身尚未让移动端或 live 构建命令消费冻结副本；desktop 非 live 消费冻结副本的接线
记录在后续 [desktop frozen build root](M04-desktop-frozen-build-root-2026-09-27.md)。本切片
仍没有接入 snapshot 构建编排、build.rs 隐藏 I/O 报告、缓存命中或实际矩阵执行。manifest 中非 Cargo workspace member
的特殊生成路径和原生 manifest 仍需后续边界处理。
