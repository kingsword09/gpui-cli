# F01 baseline CI 与父责任复核（2026-10-09）

本次接续 PR #393，原 head 为 `c241362381cd9b98d285f91c7f2fd51254aa07a4`。
本页区分 artifact 接线、报告正确性、CI 和 P-01 原生 span 责任，不因一项通过晋升 F01。

## 首轮 hosted artifact

实际下载 push run `37763658282` 和 PR run `37763708426` 的
`f01-baseline-<run>-1` artifact，保存在 ignored `artifacts/ci/<run>/f01-baseline/`。
PR checkout 的 merge revision 为 `ac48f9930042d9ab1beccac705f52941d95a6aed`，
push revision 为上述 head；两者均 `dirty=false`，原 verifier 逐样本、source/driver/workflow
hash 和 CLI binary 核验通过。三个 fixture 均各 10 warmup + 30 measurement，合计 120
samples、40 compile failures 与 40 successful recoveries、1540 spans；PR/push commands
分别为 984/1018。CLI SHA-256 均为
`b9d1518250c83be2f5f7891412a7f9a7359522c990a586413376c4ee896e0156`。

首轮 push macOS workspace 为 489 passed / 1 failed / 12 ignored；失败是 capture helper
超过现有 deadline，Windows job 被 fail-fast 取消。首轮 PR iOS smoke 已完成 doctor/build/
install 和当前 boot 的 APNs service 移除，但单次 launch 超过 180s；owned simulator cleanup
通过。两个失败与 baseline job 无关，原始失败日志及 verification JSON 保存在 ignored
`artifacts/acceptance/c241362/F01/ci-review/`；iOS raw artifact 在
`artifacts/ci/37763708426/ios-smoke-attempt-1/`。push 和 PR attempt 2 均已全部通过，
原 head 的 PR 状态为 CLEAN。未放宽任何 deadline，也未增加 app-launch retry。

## cold/warm 分类缺陷与修复

原 driver 在 `session.start()` 已成功后，把第一个 mutation warmup 标为 `startup_cold`。
首轮 artifact 的 `counter-warmup-000` 实际 build 为 `b3`；真实 setup `b1` 为 superseded，
`b2` 为成功。这会把增量编译耗时混入所谓冷启动，并遗漏 cold 组的 superseded 终态。

新增回归在原 `c241362` driver 上确定失败，错误为 `startup_cold != incremental_warm`；
原结果保存在 `ci-review/cache-label-before-fix.log`。修复不改变 10+30 mutation 样本选择：
全部 mutation samples 为 `incremental_warm`，单独保存每个真实 setup build 的 build/span ID、
duration 和原终态。每个新项目按 supervisor 单调 start time 排序，仅第一个 setup build 标为
`startup_cold`，后续 setup builds 标为 warm。startup 组使用真实 build duration，不冒充
driver elapsed；不清空全局 Cargo 或 filesystem cache，不把三个 startup cold builds 当作
30 个冷启动 measurements。verifier 对照原始 setup spans，拒绝错分类、丢失和虚构 startup。

本地 Python compile、self-test、47 项 Python 回归、fmt、CLI build、actionlint、design-doc
与 diff checks 通过。完整 macOS arm64 dirty smoke 输出位于 ignored
`artifacts/acceptance/c241362/F01/macos-arm64/attempt-13/`：120 samples、40 failures/recoveries、
1520 spans、1023 commands、7 个真实 setup builds（3 cold）；新 verifier 通过。
CLI SHA-256 为 `8c9a8cd20e0ac38f9fc2741bdb52c7db1a86e4bcfd5508f2c14a5eeec8fc2e86`。
这只证明本地修复，修复提交上的两套 clean hosted artifact 与 review/merge 仍待完成。

## P-01 native-install span 责任仍缺证据

重新读取历史 `attempt-08/ios-install-failure/result.json` 和 `spans.ndjson`：该 live attempt
实际仍为 `blocked_before_install`，失败 spans 只有 `cargo.compile`/`build`，未到 install。
修复 backtrace 后的 `/tmp/gpui-f01-nativefix-evidence/` 有成功 build、受控 install exit 73 与
cleanup 摘要，但没有本次 supervisor 的 failed native-install span。现有 F01 原始证据根中
未定位到对应 `native.install`/`ios.install`/`app.install` failed span；不据此否认 standalone
install/cleanup 已通过，但不能将其替代 P-01 的阶段计时、父子关系和实际命令关联。

精确下一动作：在新的 owned iOS simulator 上以 live supervisor 重跑受控 install failure，
保存 source identity、原命令、events、spans 和 cleanup；断言 install span failed、父 build
终态闭合、时长非负且未继续 launch。只清理本次创建的 simulator 和 session。
F01 保持 `in_progress`；scene/present/semantics 属于 P01，完整 P-02 仍需 T05/T06 责任证据。
