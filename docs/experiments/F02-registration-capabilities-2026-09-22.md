# F02：registration supported schema 声明（2026-09-22）

状态：in_progress。本子 PR 给 `.gpui/live/<session>/session.json` 增加
`supported_schema_versions`，让新 CLI 能发现 supervisor 实际可服务的控制 schema；
当前实现只声明 `[1]`，不提前声称 v2 已实现。

## 兼容性

- 新 registration 写出 `schema_version=1` 和 `supported_schema_versions=[1]`；
- 旧 registration 缺少新字段时 serde 默认为空列表，discover/request 仍按既有 v1
  路径工作；
- 字段只用于能力发现，不能绕过现有 token/session 身份认证或版本校验。

## 可复现验证

    cargo test --locked devserver::control::tests::legacy_registration_without_capabilities_still_deserializes
    cargo test --locked devserver::tests::registration_advertises_supported_schema_versions

## 尚未覆盖

v2 control command、旧 CLI→新 supervisor 的真实 archive/compatibility 矩阵及
capability negotiation 仍需后续 F02 子 PR；F02 继续保持 `in_progress`。
