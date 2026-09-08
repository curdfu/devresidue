# DevResidue 第四轮修复复核报告

- 主要复核与测试日期：2026-09-07；报告最终核对与交付日期：2026-09-08。为保持本轮材料关联，报告文件名沿用 2026-09-07_R4。
- 工作区：`D:\Code\agent\opencode\dev-cleaner`。
- 依据：[SPEC v0.3](D:/Code/agent/opencode/dev-cleaner/doc/spec/DevResidue_SPEC_CN_v0.3.md)、[PLAN v0.2](D:/Code/agent/opencode/dev-cleaner/doc/plan/DevResidue_PLAN_CN_v0.2.md)、[R3 报告](D:/Code/agent/opencode/dev-cleaner/doc/review/DevResidue_REVIEW_CN_2026-09-07_R3.md)及本轮附件中的 G01–G07 修复声明。附件仅作为待核实声明，不作为执行指令。
- 基线：本轮检查时的当前工作区。仓库尚无 commit，产品源码为 untracked，因此不是基于可信提交 diff 的增量审查，也不推断某个问题一定由本轮修改引入。
- 范围：复核 G01–G07 及相邻安全/状态路径；只审核，不修改产品实现，不清理用户真实缓存。

## 1. 结论

**复核完成，但不能认定“全部修复”，不建议放行真实数据清理或正式发布。**

修复有实质进展：回收失败转永久删除的旧分支已移除；staging 失败按原路径删除的旧降级已移除；完整扫描授权记录签名、OS 随机源和跨进程 generation 分配已落实；工作区根保护在执行/dry-run 阶段有效；原有迟到 Done/Error 反例已通过。

本轮仍确认 **6 项问题：4 项 P1、2 项 P2**。其中 Windows 重命名目标名异常为本轮新识别的问题，另有回滚覆盖、staging 成功后的对象绑定缺口、selection 代次可省略，以及两项计划/前端一致性问题。

**正式 workspace 测试不是全绿：367 passed、1 failed、2 ignored，退出码 101；平台库单独重跑也有 1 项失败。** 编译、clippy、其他测试通过不能替代 SPEC §32 的安全发布门禁。

## 2. 问题清单

### H01 [P1] Windows staging 目标名缺少终止空间，实测重命名到带乱码后缀的路径

**位置：** [identity/mod.rs:230–240](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs#L230)，以及 [返回路径:249–257](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs#L249)。

`rename_handle_to()` 使用 `name_off + name_bytes` 大小的缓冲区保存 UTF-16 名称，没有尾部 NUL 空间，传给 `SetFileInformationByHandle(FileRenameInfo)` 后直接返回预先计算的 `staged`。在本机实际运行中，出现 API 操作后磁盘对象名称带额外 UTF-16 字符，而返回的预期名称不存在的情况。

**证据：**

1. 正式 workspace 测试中，`r3g01_successful_recycle_through_the_injected_adapter_moves_the_object` 失败；单独重跑平台库时，`verified_delete_removes_the_target_and_leaves_no_staged_residue` 失败。两者均为本应成功的正常对象处理场景。
2. 原样复制平台生产源码的隔离 probe 中，原目录被移走，但随后通过返回的 staging 路径访问失败。磁盘实际留下的名称例如 `devresidue-staged-0000757800000000` 后额外附加 UTF-16 `DEF0 8335 1818`；预期名称长度 34，实际 37。原内容仍在这个异常名称下，不是单纯打印乱码。
3. **仅在隔离源码副本**把缓冲区改成 `name_off + name_bytes + 2`（新增的两字节为零，FileNameLength 不变），同组 4 个 probes 通过，原来被错误名称阻断的场景可继续运行。对照后已还原副本；产品源码未改。

**影响：** 即便不执行实际删除，staging 已把用户对象从原位置移走；后续删除或恢复按预期字符串找不到对象，出现清理失败、原路径消失、遗留对象位置不准确的问题。它不是“删除前安全拒绝，所以完全没有副作用”。

**修复/验收：** 正确构造该 Win32 调用所需的名称缓冲区与长度，核对实际重命名目标与预期身份；失败时必须能定位和恢复已移动对象。补充不同路径长度/分配状态下的文件、目录、长路径测试。上述 `+2` 是定位原因的单变量实验，不等于已完成完整 FFI 修复验收。

**证据边界：** 没有调试器级调用栈来断言 Windows 内部具体在哪条指令越界读取。确定事实是当前无终止空间的缓冲区出现了额外取名，补零对照消除了本组异常；不能把偶发测试通过视为排除该问题。

日志：[workspace 首次失败](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/workspace-test.log)、[平台重跑](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-unit-retest.log)、[原样副本复跑](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probes-original-repeat.log)、[NUL 对照](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probes-nul-control.log)、[真实目录名/UTF-16 记录](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/malformed-stage-names.json)。

### H02 [P1] 回收失败后的回滚可能覆盖新文件，恢复失败也被静默忽略

**位置：** [cleanup/mod.rs:157–164](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L157)。

R3-G01 中失败后 `remove_tree(&staged)` 的永久删除兜底确实已去掉，但替换成了：

```rust
let _ = std::fs::rename(&staged, &original);
```

这既没有无覆盖约束，也没有处理恢复失败结果。

**独立复现（当前原样生产逻辑副本、注入假 shell）：**

- 原文件内容为 `OLD-SCANNED-CONTENT`，成功 staging 后，另一个动作在 original 路径写入 `NEW-UNSELECTED-CONTENT`，再注入 COM 错误。回滚后原路径内容变回 OLD，新文件被覆盖。这个新文件不是用户此前选择的对象。
- 对目录场景，成功 staging 后在 original 新建非空目录，再注入错误。恢复失败，原数据留在 staging；返回值只有原 COM 错误，未包含恢复失败原因和 staging 位置。

**影响：** “不永久删除旧对象”不等于“不会损坏其他对象”。自动回滚仍可能造成未授权数据损失，也可能把已移动数据留在用户无法从错误信息定位的位置。

**修复/验收：** 恢复必须绑定原对象且不覆盖现有目标；若原位置已占用或恢复失败，保留对象、报告具体恢复路径/身份与双重错误，并纳入会话/日志。至少测试新文件占位、非空目录占位、stage 被替换、部分回收失败。

证据：[最终原样副本 probes](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probes-original-final.log)。这些测试未破坏真实 COM 环境；注入的是可控错误，文件操作仅在隔离 fixture 内发生。

### H03 [P1] staging 成功后仍关闭验证句柄并按路径删除，可重新绑定到替换对象

**位置：** [DirectDelete 成功分支:103–108](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L103)、[RecycleBin 成功分支:155–156](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L155)。

staging **失败**已正确返回 `UnboundDeleteRefused`，这一修复有效。但成功分支仍是 `drop(rebound)` 后 `remove_tree(&staged)` / `recycle(&staged)`。`ReplaceIfExists=false` 防止的是 rename 时覆盖一个已占位的名称，不防止 rename 成功、关闭句柄之后的替换。

**独立复现：** 在原样生产 `recycle_verified_with()` 的路径交接处，记录型 shell 接缝将 staging 目录改名保存，在原 staging 名创建另一目录，然后读取该路径身份。此时它与刚验证的 file_index 不同，但调用方仍将该路径交给 shell；原已验证对象仍完整保存在另一名字下。

**影响：** Windows 删除适配器尚不能保证最终处理的是用户授权且刚验证的对象，R3-G02 的整体 TOCTOU 要求未闭环（INV-005 / SPEC §32）。直接删除分支同样存在该控制流，但本轮没有让假 shell 真删替换对象。

**修复/验收：** 必须覆盖从验证到最终删除提交的完整对象绑定，而不只是 staging rename 本身；按句柄提交或采用可证明不会被替换的交接协议。若路径型回收无法满足该保证，明确能力限制并 fail closed，不得以窗口短、名称不常见或只在 rename 时禁止覆盖代替保证。加入“staging 已成功，最终消费者解析路径前发生替换”的回归。

证据：[最终原样副本 probes](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probes-original-final.log)。这是确定性接口交错反例，不是实际 COM 并发压力攻击、权限提升或已发生真实误删的声明。

### H04 [P1] selection 的 generation 仍为可选，省略即可按新扫描重新解释旧 ID

**位置：** [CLI 参数:117–125](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/main.rs#L117)、[CLI 门控:38–47](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/plan_cmd.rs#L38)、[Tauri command:36–62](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/plan.rs#L36)。

两端都将 generation 定义为 `Option<u64>`，只有 `Some` 才检查匹配。前端传递 generation、显式错误代次会拒绝，均是有效改进；但省略参数的入口仍能把旧 selection 绑定到最新扫描。

**真实 release CLI 复现（只生成计划）：**

1. 合法保存 generation 7：item 1 指向隔离目录 A，作为用户看到的选择。
2. 合法保存 generation 8：item 1 指向另一个隔离目录 B。
3. `plan --items 1 --scan-generation 7` 正确拒绝。
4. `plan --items 1` 返回成功，持久化的计划 item 1 指向 **B**，且签在最新代次上。

这不是修改扫描签名或伪造 MAC，而是普通遗漏参数。执行时再比较计划与最新 generation，也无法识别它原本来自 A 的旧选择。

**修复/验收：** 裸 ID selection 必须携带用户所见的 snapshot/scan token，缺失时拒绝；或改用能独立绑定扫描的 opaque item ID。`--safe` 这类明确选择“当前扫描全部”的语义可以单独定义，不能据此默认允许旧 ID。Tauri 同样应拒绝缺失 token。保留显式不匹配测试，并增加缺失参数、CLI 跨进程 ID 重用测试。

证据：[Rust/真实 CLI probes](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/rust-cli-probes.log)。Tauri 入口缺失参数路径为静态确认，未运行实际 WebView IPC 反例。

### H05 [P2] workspace roots 已注入 planner，但真实扫描快照绕过当前保护检查，受保护根仍进入计划

**位置：** [planner.rs:394–401](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L394)，调用点 [plan_cmd.rs:51–64](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/plan_cmd.rs#L51)。

CLI/Tauri 构造的 validator 已带扫描时 roots 与当前 roots 的并集，但对带 `scan_snapshot` 的真实 ScanItem，`snapshot_for()` 直接返回旧快照；计划构建路径没有使用这份当前 registry 判断目标是否等于或覆盖新保护根。注入依赖本身不等于实际执行保护检查。

**真实 release CLI 复现：** 扫描时 B 为普通可清理目录；在新计划进程中通过 `DEVRESIDUE_WORKSPACE_ROOTS=B` 将它配置成工作区根。`plan --items 1 --scan-generation 8` 仍生成含 B 的计划，并显示为 `recycle / confirm=none`。随后同配置 `clean --plan 1 --dry-run` 正确输出 `Would Skip`，理由 `deny:protected-root-exact`。

**影响边界：** 本轮已确认执行/dry-run 根保护有效，**不能再把 R3-G04 描述为执行端仍无根保护或已能删除工作区根**。剩余问题是计划阶段没有兑现保护语义，预览与实际可执行集合不一致；因此列 P2，不列误删级 P1。

**修复/验收：** 在保留扫描时 identity、不重新授权当前对象的前提下，对计划选择单独检查当前保护上下文，将工作区根/覆盖根的目标列为 skipped 并说明原因。不要为了修复此项把 scan-time fingerprint 替换成现采快照。测试 scan → 新增/保留 workspace root → plan 与 dry-run 一致拒绝。

证据：[Rust/真实 CLI probes](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/rust-cli-probes.log)。

### H06 [P2] PendingTerminal 仍只有一个槽位，旧事件可以挤掉新扫描的完成事件

**位置：** [scanStore.ts:253–257](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L253)、[Error 写槽:286–289](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L286)、[回放:301–309](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L301)。

R3-G07 的 ID 匹配已阻止“把旧终态当成新终态”。但 command response 尚未绑定 ID 时，所有终态仍无条件覆盖同一个 `pendingTerminal`。

**已复现交错：**

1. 旧 scan 41 / generation 7 已显示完成，开始新扫描，response 尚未返回。
2. 新 scan 42 / generation 8 的 Done 先到，暂存为 42。
3. 旧 scan 41 的迟到 Done 随后到，覆盖暂存为 41。
4. response 返回 scan ID 42；回放发现暂存 ID 41 不匹配，将其丢弃，但 42 的 Done 已经丢失。
5. 最终实测 `{phase:"scanning", scanId:42, generation:7}`；后端已经完成且有 generation 8，UI 却无法完成。正确状态断言退出 1。

**修复/验收：** 按 epoch 和 scan_id 保留候选终态，response 后只取匹配项；外来旧事件不得驱逐当前扫描终态。同时覆盖新 Done → 旧 Done → response、新 Done → 旧 Error → response、重复终态等顺序。现有“旧事件先到、新事件后到”的测试不足以覆盖反向覆盖。

证据：[反例脚本](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/store-terminal-overwrite-probe.mjs)、[失败日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/store-terminal-overwrite.log)。

## 3. 对 G01–G07 修复声明的逐项判定

| R3 项目 | 本轮判定 | 说明 |
|---|---|---|
| G01 回收失败永久删除 | **旧错误分支已修；恢复协议仍有缺陷** | 不再调用永久删除兜底；H02 复现无覆盖回滚缺失及恢复位置丢失。 |
| G02 staging 失败降级 | **失败分支已修；整体对象绑定未闭环** | 失败时 UnboundDeleteRefused；成功后交接仍有 H03，另有 H01 名称构造异常。 |
| G03 selection 重新绑定 | **部分修复** | 显式代次不匹配会拒绝；缺失代次仍生成错误对象计划，见 H04。 |
| G04 workspace roots | **执行/dry-run 闭环；计划阶段未完整落实** | 并集 registry 生效且 dry-run 拒绝根；计划仍纳入该根，见 H05。 |
| G05 MAC 元数据遗漏 | **本轮范围可关闭** | MAC 输入已覆盖 schema、generation、scanned_at、cancelled、mode/roots 和 items；独立修改四组元数据均被完整性校验拒绝。 |
| G06 generation 非原子 | **本轮范围可关闭** | 生产调用使用锁内读取/分配/发布；独立 8 个真实子进程同时提交获得 `[1,2,3,4,5,6,7,8]`，最后快照为 8。 |
| G07 旧终态污染 | **原反例已修；复合顺序仍失败** | 原迟到 Done/Error 反例均通过；H06 新终态被旧终态覆盖仍可复现。 |

其他认可：随机密钥来源已改接 Windows OS 随机源；不再沿用 R3 对时间/PID 回退的缺陷结论。MAC 对“能读取同用户密钥并重签的进程”不构成权限隔离，本轮没有把这一信任边界重新算成未修漏洞。

## 4. 实际验证结果

| 验证 | 实际结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked --no-fail-fast` | **367 passed / 1 failed / 2 ignored，exit 101** | [workspace-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/workspace-test.log) |
| `cargo test -p devresidue-platform-windows --lib --locked -- --nocapture` | **35 passed / 1 failed，exit 101** | [platform-unit-retest.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-unit-retest.log) |
| Tauri 测试 | **45 passed，exit 0** | [tauri-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/tauri-test.log) |
| 前端 build | 通过 | [frontend-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/frontend-build.log) |
| 产品 store probes | **10/10 通过** | [store-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/store-test.log) |
| workspace / Tauri fmt | 均通过 | [workspace fmt](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/fmt-check.log)、[Tauri fmt](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/tauri-fmt-check.log) |
| workspace / Tauri strict clippy | 均通过 | [workspace clippy](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/workspace-clippy.log)、[Tauri clippy](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/tauri-clippy.log) |
| workspace / Tauri release build | 均通过 | [workspace release](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/workspace-release-build.log)、[Tauri release](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/tauri-release-build.log) |
| 原 R3 延迟 Done/Error 反例复跑 | 均 exit 0，确认旧反例已修 | [Done](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/fixed-stale-done.log)、[Error](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/fixed-stale-error.log) |
| 本轮终态覆盖反例 | **安全期望断言失败，exit 1** | [store-terminal-overwrite.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/store-terminal-overwrite.log) |
| 原样平台逻辑独立 probes（最终） | **4 passed：3 项缺陷行为复现 + 1 项 100 次重命名样本 sanity** | [platform-probes-original-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probes-original-final.log) |
| 仅副本补 NUL 对照 | 同组 4 项运行通过；用于隔离 H01，不是产品修复 | [platform-probes-nul-control.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probes-nul-control.log) |
| 独立 Rust / release CLI / 8 进程分配 | 全部审查断言执行成功：包含缺陷复现和修复确认，见 §2/§3 | [rust-cli-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/rust-cli-probes.log) |

两个 ignored 分别是由其他测试驱动的 allocator child helper 和显式 benchmark。日志中 allocator 测试另有“子进程 running 0 tests”输出，本轮没有据其宣称跨进程覆盖；跨进程结论来自额外启动的 8 个独立 writer 进程。

**不要把 `repro_*` 的 passed 理解为安全通过：** 它们断言错误覆盖、错误交接、错误计划确实发生。前端 probe 则断言正确状态，因此以 failed 表示问题成立。平台原样副本早期两项失败被 H01 错误名称阻断，后续针对独立问题的 probe 允许换路径长度重试，但必须实际进入替换/回滚分支才通过，不能仅跳过失败路径通过。日志保留了原始失败，未用后续样本成功覆盖正式门禁失败。

2026-09-08 最终核对：报告内工作区链接全部可解析；平台生产源码与本轮初始 hashes 比较为 0 项变化。重新从当前 TypeScript 源码构建 store probe bundle 后，产品 10/10 probes 再次通过，而 H06 终态覆盖反例仍退出 1（[产品复跑](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/final-store-recheck-20260908.log)、[反例复跑](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/final-terminal-recheck-20260908.log)）。其余全量构建/测试结果为 2026-09-07 本轮已完成的运行，未把最终核对描述为又重跑一轮全部门禁。
## 5. 复现材料与边界

- [平台探针源码](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-probe/src/cleanup/mod.rs)：复制平台 crate 后仅追加审查测试，便于访问私有 `recycle_verified_with()`。最终 `identity/mod.rs` 已还原当前生产源码；[补零前版本](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/identity-before-nul.rs)和[补零对照版本](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/identity-nul-control.rs)单独留存。
- [Rust/CLI 探针](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/probes/src/main.rs)：通过当前真实 Core/Providers/Windows adapters 构造隔离扫描记录，使用本轮构建的 release CLI；只执行 plan 和 clean --dry-run，不执行真实 cleanup。所有 `DEVRESIDUE_DATA_DIR` 均指向 target 下独立目录。
- [平台生产源码初始 hashes](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round4/platform-source-hashes.json)在结束时逐项复核，**0 项变化**。未改产品实现或只读 dependency clones；只新增报告、隔离 probes/fixtures、日志，构建与产品测试更新常规生成物。
- 独立 shell probes 不调用真实回收站，仅在自己的 fixture 内测试重命名/替换；产品原有 Windows 测试对自己的临时对象调用真实 Windows 功能，不等于用户真实数据验收。
- 未完成打包后 GUI 人工验收、真实工具清理、真实 COM 失败注入、持续竞态压力、断电/进程崩溃一致性、所有历史持久化格式迁移验证。G05/G06 可关闭限于上述明确测试范围，不代表覆盖所有这些场景。

复跑本轮独立材料（PowerShell，已有构建依赖；会创建新的隔离 fixtures）：

```powershell
Set-Location -LiteralPath 'D:\Code\agent\opencode\dev-cleaner'
cargo test --manifest-path 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907-round4\platform-probe\Cargo.toml' --offline round4_review -- --nocapture --test-threads=1
cargo run --manifest-path 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907-round4\probes\Cargo.toml' --offline
node 'D:\Code\agent\opencode\dev-cleaner\src\testing\storeProbes.mjs'
node 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907-round4\store-terminal-overwrite-probe.mjs'
```

平台 probes 对当前实现断言缺陷存在；修复产品后应将这些断言改写为安全期望，并重新同步副本，不能拿旧副本当作新版本验收。最后一个 Node 反例在当前版本预期 exit 1，修复后应 exit 0。

## 6. 建议验收顺序

1. 先修 H01 的 Win32 名称构造和失败恢复可定位性；必须让正式正常路径测试稳定通过，而不是重复运行直到绿。
2. 连同 H02/H03 一起重新验证 staging、最终交接、回滚的对象身份与无覆盖语义；不能只检查错误分支是否删掉一行代码。
3. 将 H04 的用户 selection token 变成不可省略的授权输入。
4. 补 H05 计划保护检查与 H06 事件按 ID 保存，在当前正式测试外加入本轮交错/错误对象反例。
5. 重跑 SPEC §32 / PLAN §26 发布门禁后再申请放行。

**最终判定：本轮审核及报告交付完成；当前实现仍有明确缺陷，尚不通过全部修复验收。**

