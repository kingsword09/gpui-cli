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
这只证明本地修复；随后 `5cfc419` 的 PR/push runs `37863074479` / `37863069334` 各
14 jobs 全绿，两套完整 raw artifact 下载、ZIP integrity 与新 verifier 均通过。PR merge
revision 为 `99466d93ba30015c5ffc5907baf78d87a3894f6c`，push revision 为
`5cfc4191a3e504c10460c33125677f74282dbe16`，均 `dirty=false`；各 120 samples、40 failures/
recoveries、6 setup builds / 3 cold，PR/push spans 为 1539/1540、commands 为 1002/1028，
CLI SHA-256 均为 `b9d1518250c83be2f5f7891412a7f9a7359522c990a586413376c4ee896e0156`。
原始 report、ZIP、artifact metadata 和 verifier 输出位于 ignored `artifacts/ci/<run>/`。
PR #393 已于 2026-10-09 squash 为 `3db1202`。下载的代理 stall 和有界直连 range retries
不是 workflow 或应用失败，不改变 doctor/launch 或样本政策。

## 历史复核：P-01 native-install span 责任缺证据

重新读取历史 `attempt-08/ios-install-failure/result.json` 和 `spans.ndjson`：该 live attempt
实际仍为 `blocked_before_install`，失败 spans 只有 `cargo.compile`/`build`，未到 install。
修复 backtrace 后的 `/tmp/gpui-f01-nativefix-evidence/` 有成功 build、受控 install exit 73 与
cleanup 摘要，但没有本次 supervisor 的 failed native-install span。现有 F01 原始证据根中
未定位到对应 `native.install`/`ios.install`/`app.install` failed span；不据此否认 standalone
install/cleanup 已通过，但不能将其替代 P-01 的阶段计时、父子关系和实际命令关联。

精确下一动作：在新的 owned iOS simulator 上以 live supervisor 重跑受控 install failure，
保存 source identity、原命令、events、spans 和 cleanup；断言 install span failed、父 build
终态闭合、时长非负且未继续 launch。只清理本次创建的 simulator 和 session。
以上为当时恢复条件，后续补跑结果见下一节。F01 保持 `in_progress`；scene/present/semantics
属于 P01，完整 P-02 仍需 T05/T06 责任证据。

## 新 live install span 独立核验

在干净 `5cfc419`、macOS arm64 / iOS 18.4 的新 iPhone 16e simulator
`A4E3EAFA-197E-4765-898A-CB4B49079958` 上，重新生成 iOS-only `f01nativefix` 并运行
`gpui run ios --live --device <owned-udid>`。复用此前隔离 fixture 的 Cargo dependency cache，
不据此声称 cold build 或性能结果。真实 Cargo 和 Xcode build 成功；仅该进程的 xcrun shim
在 `simctl install` 返回受控 exit 73，未委托真实 install，也未执行 launch。

原始 spans 实际使用 `name=device.install`、`attributes.stage=ios.install`。session 为
`live-29e4ffc971ba4a8af31d16293431ce9e`，build 为 `b1`，install `sp14` 为 failed，时长
20.134041 ms，parent build `sp6` 为 failed；原始单调 start/end/duration 相符，child 包含于
parent，stage.finished 同一 build 的 success=false，没有该 build 的 app.launch span。
supervisor 在本轮 q 结束，后续 `b2` cancelled；session lifecycle=ended，lease owner 已移除、
lock 按实现保留，owned simulator shutdown/delete 均 exit 0 且 inventory 确认删除。

原实验脚本错误要求 supervisor error 字符串包含 `73`，因此 `result.json` 保留为 fail。
实现只在错误中保留实际 install 命令与 failed；exit 73 是 shim 的受控返回值，不要求进入
supervisor 字符串。独立核验改为交叉检查原 shim、argv 日志、stage event、父子 spans 和
cleanup，不修改原始失败结果，产出 `independent-verification.json=pass`。
app-container 检查未在删除前执行，明确记 not_run；未委托真实 install 的事实来自 shim，
不冒充额外 device 查询。原始目录为 ignored
`artifacts/acceptance/5cfc419/F01/macos-arm64/attempt-14/`，包含 source/fixture hashes、
Cargo.lock、commands、events、output logs、spans、live.log 与 evidence SHA-256 清单。

这补齐 F01/P-01 本机 simulator 的 failed-install span 责任，不代表 scene/present/semantics、
跨平台 GUI 或 physical-device 验收。父责任继续复核：现有 hosted artifact 保存当前 CLI，
但尚未定位原设计基线 `6d091b6` 的旧 CLI binary 与可复验 v1 scaffold 归档；不得仅以
当前二进制内嵌模板替代旧材料。下一个 F01 有界切片为固定该 immutable revision 的旧 CLI/
v1 scaffold/source/toolchain/hash 材料归档，比较与真实在线兼容属于 F02，非本切片目标。

## 用户指定有界切片：旧 CLI/v1 材料归档

用户明确要求先完成当前归档切片，不扩张其他父任务。固定
`6d091b661d7ef82115e88cf142c7ff252b593864` 是本轮实施方案；路线要求保存基线材料，
不是要求每次重建旧版本成为新的验收门槛。本轮复用现有 baseline job/artifact，没有新增 gate。

归档 driver 从 Git archive 原始源码，在当前 workspace 外的 temporary source root 执行
locked Cargo build 和 3 个旧协议 unit tests，保存原 CLI Cargo.lock、旧 binary 和其实际版本，
实际执行旧 init 生成 macOS/iOS/Android v1 scaffold。记录 producer commit/dirty、Rust/Cargo、
host、原命令/单调 elapsed 与完整 stdout/stderr；逐文件 SHA-256 校验，包括隐藏文件。
verifier 还对照 immutable Git archive、原 lockfile 和原 v1 template bytes，拒绝未知来源、
文件篡改/symlink、复用旧 evidence directory；失败/timeout 保留日志和 fail manifest。

本机真实源码构建/三端 scaffold/protocol tests 已通过。早期在当前 workspace 内解包的 build
被 Cargo 的父 workspace 规则拒绝，保留失败；driver 改为外部 temporary source，无需编辑旧
Cargo.toml。第一次 driver smoke 的 Android 校验路径缺 `gradle/`，保留失败并修正 verifier；
后续完整 smoke 通过。原始实验材料在 ignored
`artifacts/acceptance/6d091b6/F01/macos-arm64/legacy-materials-2026-10-09/`，driver smoke 在
`artifacts/acceptance/5cfc419/F01/macos-arm64/legacy-driver-02/` 与
`artifacts/acceptance/3db1202/F01/macos-arm64/legacy-driver-03/`。

归档是历史源码在记录的现有工具链下重建，不冒充当年的 release binary；不构建/启动 generated
应用、不修改 SDK/许可/签名，不声明 F02 在线兼容或 GUI/device acceptance。
最终本地 58 项 Python 回归、Python compile、actionlint、fmt、design-doc 与 diff checks 已通过；
driver-04 的完整真实归档和 verifier 通过，保存在 ignored
`artifacts/acceptance/3db1202/F01/macos-arm64/legacy-driver-04/`：121 个材料文件，旧 CLI SHA-256
为 `91e8051f4039b651f551e5c352a65b5a66b44bbd09ba520d0f5951d7b40a6db3`，source tar SHA-256
为 `b353f85cdf6faf4de0bba0c0cdc2b30582c9850bbbe9df386f4b43840d9d46a7`。
producer 为 `3db1202` / dirty，不能代替 clean hosted artifact。
实现已接入 CI artifact 的 `legacy/`，另保留 `legacy-verification.json`；仍待新切片
CI/raw artifact 与 review/merge，完整 workspace/clippy/build 验证单独保存。
Clippy 和普通 build 通过；默认并行 workspace 为 485 passed / 5 failed / 12 ignored，
涉及三项 process/helper cleanup 等待、mobile native-log deadline 和 command probe Unknown。
五项分别隔离重跑均通过，串行完整 workspace 通过（主二进制 490 passed / 12 ignored，
全部 integration/protocol/xtask targets 通过）。原并行失败、定向复跑和串行结果分别保存在
ignored `artifacts/acceptance/3db1202/F01/archive-closeout/`，不把串行成功冒充并行稳定性。
本切片未修改任何 Rust 实现或 Cargo dependencies；CI 仍按原默认并行标准运行。
F01 保持 `in_progress`，35 项计数不变；这次仅以归档切片的分层证据闭合为出口。

### 首轮 hosted artifact 漏传隐藏材料

干净本机归档 `artifacts/acceptance/c6c8564/F01/macos-arm64/legacy-clean-01/` 已通过，
producer=`c6c8564b3775f7895396510d78a5703b2f750b22`、dirty=false，121 个材料文件。
PR #394 首轮 push run `37907124415` 的 14 jobs 全绿；PR run `37907132119` 的 baseline job
也成功，但实际下载并完成 ZIP CRC 核验后，独立 legacy verifier 失败。
expected PR checkout=`89d9134b4c3edce4749ae66567bc8c789674ae5f` 来自独立 PR API，
不是取 artifact 自报 revision。当前 baseline verifier 仍通过：120 samples、40 failures/recoveries、
1540 spans、1007 commands、6 setup builds / 3 cold、dirty=false。

旧材料 manifest 记录 121 个文件，下载产物仅 119 个；缺失 `project/.gitignore` 和
`project/mobile/android/.cargo/config.toml`，无 extra 或 changed 文件。
上传 step 未显式保留隐藏文件，job 内 verifier 在上传之前运行，不能发现发布产物丢失。
修复仅为既有 F01 artifact 设置 `include-hidden-files: true`，范围仍限于该 evidence root；
隐藏材料是历史 scaffold 的 ignore/target 配置，不上传整个 checkout 或用户配置。
新增 workflow 回归在修复前确定失败，原日志为 ignored
`artifacts/acceptance/3db1202/F01/archive-closeout/hidden-upload-before-fix.log`。
原 ZIP、artifact metadata、独立 expected identity 和当前 baseline verification 保留于
`artifacts/ci/37907132119/`；待修复提交的两套完整 clean artifact 与全部 jobs 重新核验，
不重写首轮失败，也不放宽任何运行时 deadline。
