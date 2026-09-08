# DevResidue 第三轮修复复核报告

- 复核日期：2026-09-07。
- 工作区：`D:\Code\agent\opencode\dev-cleaner`。
- 依据：[SPEC v0.3](D:/Code/agent/opencode/dev-cleaner/doc/spec/DevResidue_SPEC_CN_v0.3.md)、[PLAN v0.2](D:/Code/agent/opencode/dev-cleaner/doc/plan/DevResidue_PLAN_CN_v0.2.md)、[第二轮报告](D:/Code/agent/opencode/dev-cleaner/doc/review/DevResidue_REVIEW_CN_2026-09-07_R2.md)。重点为 SPEC §11、§31（INV-001..013）、§32 和 PLAN §26。
- 基线：本轮验证时的当前工作区。`git rev-parse --verify HEAD` 失败，仓库尚无 commit，产品源码为 untracked；本报告不是基于可追溯提交 diff 的增量审查。
- 范围：复核 R2 修复及相邻执行路径，只审核，不修改产品实现。新增本报告与 target 下隔离审查材料；构建及产品测试会更新常规生成物。

## 1. 结论

**本轮审核已完成，但当前实现仍不建议通过真实数据清理验收或正式发布。**

修复确有实质进展，不是“上一轮问题完全没改”。R2 的 **F04、F05、F06、F08、F09** 可在本次检查范围内关闭；F01、F03、F07、F10 只完成了部分闭环，F02 的对象绑定问题仍存在。修复 Windows staging 协议时，还引入了回收失败后未经授权永久删除的新问题。

本轮归并为 **7 项：4 项 P1、3 项 P2**。P1 涉及清理模式授权、错误对象清理或根保护；P2 涉及完整性、代次分配、前端状态一致性。尤其 G01/G02 不能用“全部测试通过”“竞态窗口很短”或“Phase 0–6 范围”豁免。

## 2. 问题清单（按严重度）

### G01 [P1] 新问题：回收站失败后，自动升级为永久删除

**位置：** [Windows cleanup/mod.rs:130–138](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L130)。

`recycle_verified_impl()` 先将已验证对象重命名至 staging，然后调用回收站 API。当回收操作返回错误时，代码直接调用 `remove_tree(&staged)`，随后仍返回原回收错误。

**触发与影响：** 用户授权的是 RecycleBin；若回收操作失败、对象仍留在 staging，且直接文件删除可以成功，平台层会将其永久删除。CleanupEngine 看到的却仍是失败。它同时破坏了清理模式授权、可恢复语义以及审计结果与实际副作用的一致性。这里不是清理一个可丢弃的副本，staging 对象就是用户选中的原对象。

**证据边界：** 确定的静态控制流，未通过人为破坏真实回收站来复现数据损失，也未声称所有回收错误都会导致永久删除。

**修复与验收：** 回收失败必须保留对象，安全地恢复原位或记录可恢复的 staging 位置并报告失败；不得静默转成 DirectDelete。注入 COM/回收错误，断言永久删除端口调用为零、内容仍可恢复、会话正确记录状态。

### G02 [P1] R2-F02 未闭环：verified 删除仍存在关闭句柄后按路径删除的降级

**位置：** [Windows cleanup/mod.rs:95–110](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L95)、[127–143](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L127)、[153–161](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L153)。

当前确实新增了同一已验证句柄上的 staging rename，比旧实现有进展。但两个分支仍未闭环：

1. staging rename 失败后，显式 `drop(rebound)`，再对原始路径调用 `remove_tree(path)` / `recycle(path)`。这直接恢复了 R2 指出的 verify → close → path 操作协议，而且成功降级仍返回成功。
2. staging 成功后同样先关闭绑定句柄，再按 staging 路径删除。该名称实际由 PID + 单调 Atomic nonce 组成，不是注释所称不可预测随机名；实现也没有建立受保护的 staging 命名空间来排除关闭句柄后的替换。

**触发与影响：** 最终路径解析与前一次句柄身份验证之间，目标名称仍有机会绑定到另一个对象。rename 失败不得成为削弱 TOCTOU 防护的理由。即使换用随机名字，也不能直接推出对象绑定已成立。

**证据边界：** 本轮为静态协议审查，未做最终窗口的动态竞态复现。现有 Windows 验证测试通过，只能证明它们覆盖的替换时序，不能证明 staging 失败及 stage 名称替换窗口安全。

**修复与验收：** 协议无法保持绑定时必须 fail closed。明确说明原路径、父目录、stage 对象的替换威胁与防护；补 staging 失败、关闭句柄后的 stage 替换、原路径替换等测试，断言新对象永不被清理。该项对应 SPEC §31 INV-005、§32 TOCTOU Replacement，以及 PLAN §26 不得弱化的约束。

### G03 [P1] R2-F03 部分修复：旧 selection 在新扫描中被重新绑定，计划 generation 无法识别旧用户意图

**位置：** [CLI plan_cmd.rs:20–41](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/plan_cmd.rs#L20)、[ScanCtx ID 分配:27](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_ctx.rs#L27)、[256](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_ctx.rs#L256)。Tauri 创建计划接口也只有裸 ID：[plan.rs:31–35](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/plan.rs#L31)。

**有效修复：** 已建计划携带 `scan_generation`，CLI/Tauri 执行传入当前代次，旧已建计划能够被拒绝。

**仍失败的场景：** 用户从扫描 A 记下 item 1；另一个 CLI 进程扫描 B，进程内静态计数器重新从 1 分配 ID。此时用户再用原 selection `[1]` 创建计划，CLI 加载的是最新 B，并将 B 的当前 generation 写到新计划。执行代次检查当然通过，但用户选过的是 A，不是 B。

**本轮独立复现：** 保存 A/id1/gen7 后保存 B/id1/gen8；将旧 selection 交给与当前入口相同的 planner/load 流程。生成计划为 gen8，Engine 以 expected gen8 执行，记录型假 DeletePort 收到 `recycle:C:\review\not-selected-b`。未实际删除，也未声称这是完整 CLI 跨进程端到端测试；跨进程 ID 重置来自生产分配代码。

**修复与验收：** 从用户选择请求开始绑定 `(scan_generation, ScanItemId)`，并在解析 selection 前核对代次，或使用跨进程不复用的扫描项标识。不能仅在输出计划中补上“此刻最新代次”。增加两个真实 CLI 进程的 ID 复用回归；旧请求必须失败，不能被解释为新对象。

### G04 [P1] R2-F07 部分修复：执行阶段未注入 workspace roots

**位置：** [CLI support.rs:93–97](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/support.rs#L93)、[Tauri support.rs:83–87](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/support.rs#L83)。执行调用点：[CLI clean_cmd.rs:145](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/clean_cmd.rs#L145)、[Tauri plan.rs:129](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/plan.rs#L129)。

**有效修复：** CLI/Tauri 缓存专用扫描入口已注入工作区保护根，已有对应测试通过。

**剩余缺口：** 计划/执行共用的 `build_validator()` 仍以 `ProtectedRootRegistry::build(env, vec![])` 构造校验器，没有注入扫描保存的 workspace roots，也没有解析执行环境的工作区配置。工具 scope preflight 只核对实时查询结果与冻结 cache root 相等，不会因此自动获得缺失的工作区保护。

**具体场景：** X 在扫描时作为 cache root 合法；之后用户在新的执行进程中将 X 配置为 workspace root，工具查询仍返回 X、文件 identity 也未变。当前执行端既看不到新增工作区保护，scope equality 也不报变化，因而不能按 SPEC §11 拒绝这个 Workspace Root。这里指新进程读取的新配置，不是假设运行中进程能自动看见其他进程改写的环境变量。

**证据边界：** 静态生产调用链确认，未做完整 Tauri 端到端场景。

**修复与验收：** 明确扫描保护上下文与当前执行保护上下文的合并规则，将必要 roots 注入真实 SafetyValidator，并让 dry-run/execute 共用。测试 scan → 更新执行配置 → plan/execute：工作区本身及会覆盖它的过宽目标应 fail closed。对应 SPEC §11、INV-011 和 Root Protection 发布门禁。

### G05 [P2] 扫描完整性签名遗漏了参与安全决策的元数据

**位置：** [scan_store.rs:155–159](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L155)、[182–203](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L182)。

`mac_input()` 只签 `snapshot.items`，明确排除 generation、cancelled、mode 等字段。然而它们并不是纯显示信息：generation 用于旧计划门控；cancelled 用于 CLI partial-scan 拒绝；mode 决定 Tauri planner 是否要求真实扫描快照，且内含 workspace roots。

**本轮独立复现：** 不读取密钥、不重算 MAC，仅对合法保存的 JSON 分别修改 generation、cancelled、mode、workspace_roots，四种修改均被 `scan_store::load()` 接受，且改写值成为返回快照的一部分。

**影响边界：** 这证明安全决策输入不在完整性校验范围内，不等于仅切换 mode 就必然完成一次任意删除。攻击前提是可修改持久化扫描记录；不是远程攻击或权限提升结论。

**修复与验收：** 对完整的、带 schema/version 的授权记录进行认证，把参与门控的元数据与 items 一起绑定。对上述字段逐一、组合篡改，load 或后续授权必须拒绝；明确历史记录迁移/要求重扫策略。另见 §4 的同用户密钥信任边界。

### G06 [P2] generation 的 read → increment → save 不是原子分配，多 writer 会拿到相同代次

**位置：** [scan_store.rs:82–88](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L82)、[163–174](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L163)。

`next_generation()` 独立读取最后快照并加一，保存由调用方稍后执行。两个 writer 同时基于 N 读取，都会得到 N+1。与 G03 不同，即使 selection 开始携带 generation，该分配问题仍会使两个不同扫描共享同一代次。

**本轮独立复现：** 初始 gen7，两个线程各调用当前 `next_generation()` 后在 barrier 汇合，再顺序保存，实际为 `[8, 8]`，最终仍是8。这是两个独立 writer 的确定性交错测试，不是已运行的跨进程压力测试。

此外，当前任何 load 错误都 `unwrap_or(0)`，下一代会重置为1；保存又直接覆盖目标 JSON，缺少原子发布协议。不能把损坏/瞬时读取失败与“第一次扫描不存在文件”等同。

**修复与验收：** 在跨进程互斥/事务下分配并提交代次，或改为不依赖共享自增的全局唯一扫描 token；原子发布完整快照，区分不存在与损坏。两个进程并发时，已发布扫描身份必须唯一，旧授权不能因重复/重置代次重新生效。

### G07 [P2] R2-F10 部分修复：延迟的旧 Done/Error 会被当成新扫描的终态

**位置：** [scanStore.ts:104–125](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L104)、[230–280](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L230)、[290–337](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L290)、[358–371](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L358)。

**有效修复：** 新扫描的 Done/Error 先于 command response 到达时，当前会暂存终态，因此 R2 中连续第二次扫描卡在 scanning 的原反例已修。

**新缺口：** 暂存记录只保留当前本地 epoch，丢掉事件自带的 scan_id；response 绑定新 ID 后回放时也不核对事件 ID。“旧扫描不能再产生事件”并不能排除已经产生的旧事件延迟送达。

**本轮对真实 store 的隔离复现：**

1. 旧扫描 id41/gen7 已终态，开始新扫描并清空 scanId。
2. 旧 id41 的终态延迟到达，被暂存到当前 epoch。
3. 新命令返回 id42，但其 worker 尚未完成、持久化结果仍是 gen7。
4. 旧 Done 被回放后得到 `{"phase":"done","scanId":42,"generation":7}`，本应仍为 scanning；旧 Error 的独立场景得到 `{"phase":"error","scanId":42,"generation":7}`。

两个脚本均以“必须继续 scanning”为断言，实际 exit 1，确认缺陷。它们使用产品测试脚本新生成的真实 store bundle，只替换 backend/event I/O。

**修复与验收：** PendingTerminal 保存原始 scan_id，response 后精确匹配再回放；不应把不同 ID 的事件重新标记为当前 epoch。异步 finalise 也应校验本次 scan/epoch，避免过期读取改写新状态。覆盖早到新终态、迟到旧终态、已知不同 ID、重复终态及异步 finalise 交错。

## 3. 第二轮十项问题的闭环状态

| R2 项 | 本轮状态 | 已确认进展与剩余条件 |
|---|---|---|
| F01 授权对象/扫描来源 | 部分修复 | Engine 已绑定原 path、identity、risk、mode、command；独立五场景计划篡改均被拒绝。扫描授权记录的完整性/信任边界仍见 G05、§4。 |
| F02 最后一次 TOCTOU | 未闭环 | 新增 handle staging，但失败回退旧协议，成功路径仍有 path 窗口；见 G02，另引入 G01。 |
| F03 generation / ID | 部分修复 | 旧已建计划被正确拒绝；旧 selection 及并发分配仍有 G03/G06。 |
| F04 protected glob 根相等 | 本轮范围内可关闭 | Engine 以双向包含/相等判定保护区域交集，相关正式测试通过。 |
| F05 真实扫描缺失快照 | 本轮范围内可关闭 | Tauri 指纹失败项从 kept 中剔除，不再流出/持久化；真实 planner 返回 MissingScanSnapshot。 |
| F06 规则重分类来源 | 本轮范围内可关闭 | ScanItem 保存 classification_rule_id，重分类写入 rule slug，执行复验规则来源；移除规则回归通过。 |
| F07 workspace roots | 部分修复 | 扫描入口已修，CLI/Tauri 对应测试通过；执行端仍缺根上下文，见 G04。 |
| F08 uv argv | 本轮范围内可关闭 | 当前使用 `uv cache clean --cache-dir <path>`；未执行真实 uv cache 清理。 |
| F09 dry-run scope | 本轮范围内可关闭 | 非破坏性 scope preflight 已移到 dry-run 分支之前，scope drift 正式测试通过。 |
| F10 扫描终态竞态 | 部分修复 | 产品 7/7 store probes 通过，原 early-terminal 场景已修；延迟旧事件错误归属见 G07。 |

“可关闭”指 R2 描述的具体缺口已在本次代码检查/测试覆盖范围内修复，不代表对全部 Provider、所有保护规则组合或整个 GUI 作穷尽证明。

## 4. 扫描记录作为外部命令授权源的剩余信任边界

R2-F01 除了计划对象篡改，还要求不能把另一份可编辑 JSON 直接当作可信命令来源。本轮增加 HMAC 能检测未重签的 items 修改，但应准确描述它的边界：

- [scan_store.rs:100–113](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L100) 将 key 保存在同一数据目录，采用 user-default ACL。若威胁模型包含可读该目录的同用户进程，该进程可读取 key 并重新签名。
- 独立 probe 在隔离目录读取 key，将扫描 action 改为 `INERT-NOT-EXECUTED-REVIEW.exe` 并重签，load、planner、Engine 都接受，最终仅由假端口记录该命令；**没有启动该命令**。
- 当前 Engine 会对比 plan 与 scan 的 command，但两者一致不等于命令来自可信内置清理策略。无 scope 的命令也不进入工具 scope 查询。
- [scan_store.rs:116–142](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L116) 的 Windows 密钥生成是 time + pid + address 加 SplitMix 展开，不是函数说明所称 OS CSPRNG。本轮未做密钥预测攻击，不能将它写成已破解密钥。

**判定：** 不将上述 probe 包装成权限提升或另一项独立远程漏洞。若同用户可写状态不在防御范围，应在 SPEC/威胁模型中明确，不能声称 HMAC 已解决该范围的任意命令注入；若属于范围，则需要可信策略重新构造/严格校验命令，以及与状态写入者真正隔离的授权边界。修复不能只再加一层使用同目录可读 key 的签名。

## 5. 验证结果与材料

材料根目录：[第三轮验证材料](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3)。

| 验证 | 实际结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked --no-fail-fast` | **352 passed / 0 failed / 1 ignored** | [workspace-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/workspace-test.log) |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | **43 passed / 0 failed** | [tauri-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/tauri-test.log) |
| `npm run build` | 通过 | [frontend-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/frontend-build.log) |
| `node src/testing/storeProbes.mjs` | **7/7 passed** | [store-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/store-test.log) |
| workspace / 独立 Tauri fmt check | 均 exit 0 | [fmt-check.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/fmt-check.log)、[tauri-fmt-check.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/tauri-fmt-check.log) |
| workspace / 独立 Tauri all-targets clippy `-D warnings` | 均通过 | [workspace-clippy.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/workspace-clippy.log)、[tauri-clippy.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/tauri-clippy.log) |
| workspace release build `--locked` | 通过 | [workspace-release-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/workspace-release-build.log) |
| 独立 Tauri release build `--locked` | 通过 | [tauri-release-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/tauri-release-build.log) |
| 本轮独立 Rust probes | **6 tests passed**：2 个修复确认测试（含五场景矩阵），4 个缺口/信任边界复现测试 | [rust-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/rust-probes.log) |
| 延迟旧 Done / Error 独立 store 反例 | **两者均复现**，安全期望断言均 exit 1 | [Done 日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/store-stale-event-probe.log)、[Error 日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/store-stale-error-probe.log) |

**不得把独立 Rust probes 的“passed”算成安全验收通过。** `fixed_*` 断言应拒绝且确实拒绝；`repro_*` 则断言当前错误放行/重复分配/可重签行为存在。前端独立反例采用相反形式：断言正确状态，实际失败。两种形式都在日志中明确区分。

审查源码：

- [Rust probes](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/probes/src/lib.rs)
- [延迟旧 Done probe](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/store-stale-event-probe.mjs)
- [延迟旧 Error probe](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round3/store-stale-error-probe.mjs)

复跑独立材料（PowerShell）：

```powershell
Set-Location -LiteralPath 'D:\Code\agent\opencode\dev-cleaner'
cargo test --manifest-path 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907-round3\probes\Cargo.toml' --offline -- --nocapture --test-threads=1
node 'D:\Code\agent\opencode\dev-cleaner\src\testing\storeProbes.mjs'
node 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907-round3\store-stale-event-probe.mjs'
node 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907-round3\store-stale-error-probe.mjs'
```

最后两个命令在当前版本预期 exit 1，修复后应正常通过。Rust harness 的 API/字段适配编译错误已修正，表中记录最终成功运行，不是据未编译源码推测的结果。

## 6. 未覆盖与后续验收

- 未针对用户真实缓存或开发数据运行清理；独立清理/命令反例均使用记录型假 DeletePort。已有产品 Windows 测试的 fixture 操作不等于真实数据验收。
- 未进行打包后 GUI 人工验收、真实 uv 清理、实际回收站失败注入、最终删除窗口压力攻击、完整多进程并发/崩溃发布测试。G01/G02/G04 的证据边界已在各项说明。
- 未修改只读 dependency clones，未修产品代码。未重跑 R2 的全部独立 probes；对其结论按当前代码、正式回归和本轮新 probes 做复核。
- CLI 对 cancelled snapshot 的默认拒绝与 Tauri 行为是否应一致，建议明确产品策略；因未找到“所有取消扫描结果必须禁止计划”的明确要求，本轮不将其另列确定缺陷。

**建议验收顺序：** 先消除 G01 的未经授权永久删除和 G02 的不安全降级；再闭合 selection/扫描身份/保护上下文（G03/G04/G06），补齐授权记录与命令信任边界（G05/§4），修正事件关联（G07）。把反例改为正式的“必须拒绝/必须保持正确状态”回归，重跑 SPEC §32 门禁后再申请通过。

**最终判定：本轮审核交付完成；安全问题尚未全部修复，当前版本不建议放行真实数据清理或正式发布。**
