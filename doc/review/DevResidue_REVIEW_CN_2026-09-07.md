# DevResidue 文档与实现审查报告

- 审查日期：2026-09-07
- 工作区：`D:\Code\agent\opencode\dev-cleaner`
- 基线：当前工作区全量实现。Git 尚无 commit，源码均为 untracked，无法用提交号固定本轮基线；这不是 diff review。
- 依据：[SPEC v0.3](D:/Code/agent/opencode/dev-cleaner/doc/spec/DevResidue_SPEC_CN_v0.3.md)、[PLAN v0.2](D:/Code/agent/opencode/dev-cleaner/doc/plan/DevResidue_PLAN_CN_v0.2.md)、[AGENTS.md](D:/Code/agent/opencode/dev-cleaner/AGENTS.md)。
- 本轮只审查，未修改产品源码；新增本报告、隔离复现脚本及日志，构建产生了常规输出。

## 1. 结论

**项目已有实质实现，但不能验收为“所有开发工作已完成”，也不满足安全清理产品的发布条件。**

四个 Rust crate、CLI、独立 Tauri 壳、React UI、规则系统、Windows 文件系统能力、清理计划与日志、Kondo 接入和多类 Provider 均已落地，构建和现有测试也能通过。问题不是“没有开发”，而是若干安全边界只在单个模块内部成立，跨 Scan → Plan → Execute、父子目录及前后端事件时不成立。

本轮列出 **8 项 P1、3 项 P2**。其中关键问题直接关联 SPEC §31 的安全不变量以及 §32 的发布阻断要求；即使只验收 AGENTS 所称的 Phase 0–6，也不能通过 Phase 5/6 的安全验收。建议修复并补齐回归测试前，仅将此版本用于隔离测试数据的验证，不批准对真实开发目录/缓存开展正式清理，也不发布给普通用户。

严重度：P1 = 应优先修复、涉及安全契约或错误对象清理；P2 = 正常功能/交付缺陷。未将需要本地可写文件或特定竞态条件的问题夸大为无条件的 P0、远程攻击或权限提升。

## 2. 问题清单

### R01 [P1] 安全快照在生成 Plan 时才采集，无法发现 Scan → Plan 之间的替换

**位置：** [planner.rs:265–273](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L265)、[scan_item.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/domain/scan_item.rs)。

`ScanItem` 没有保存扫描时的文件 identity / 完整安全快照；`snapshot_for()` 在用户选择之后调用 `validator.capture()`，成功就把当前对象作为基准。因此扫描时识别的是对象 A，随后同一路径被替换成 B，生成 Plan 时会“认可”B，清理前验证也能通过。现有 capture → validate 测试不能证明真实 scan → plan → cleanup 链路安全。

- **证据：** 独立 probe 把 identity 从 100 改为 200 后生成 Plan，Plan 保存 200，Engine 最终调用假删除端口。
- **规格：** SPEC §16 的 Scan → Snapshot → Selection → Plan 顺序；PLAN Phase 5 明确要求覆盖 scan 后 replace / rename / 删除重建。
- **修复验收：** 在扫描识别时建立并持久化对象身份与识别依据，Plan 必须携带原始快照，不能静默用当前对象替换原始授权；scan 后替换、rename/recreate、重启后读取旧 scan 等场景必须拒绝并要求重新扫描。

### R02 [P1] 可编辑 Plan JSON 中的执行方式、命令和确认级别被直接信任

**位置：** [plan_store.rs:168–174](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/plan_store.rs#L168)、[engine.rs:120–125](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L120)、[engine.rs:292–307](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L292)。

PlanStore 只做 JSON 反序列化。Engine 虽会重新推导部分路径的风险，但直接信任 `mode`、`external_command` 和 `confirmation`：正常的回收站计划可被改成外部命令或直接删除；Review 项也可只改确认字段便绕过确认。按 ID 加载一个 JSON 文件，并不等于已经建立可信执行授权。

- **证据 A：** 真正调用 PlanStore 保存并改写文件后，再经 PlanStore 加载；伪造的 `review-placeholder.exe` 到达假 ExternalCommand 端口。没有运行该程序。
- **证据 B：** Review 风险保持不变、当前规则也仍判定 Review，仅把 confirmation 改成 None，默认无确认执行到达假删除端口。
- **边界：** 这是同一用户可写本地计划文件下的安全契约缺陷，不是已经证明的远程执行或提权；Engine 的 F12 注释本身已将 persisted plan 视为 user-editable 并试图防篡改。
- **规格：** SPEC §19、§22；INV-010/013 的授权链，以及“ExternalCommand 仅来自可信内置来源”。
- **修复验收：** 执行时从可信来源重新推导动作、风险与确认要求，校验来源/策略版本及目标绑定。不可把反序列化的任意命令描述符当作授权。增加 mode/args/executable/confirmation/path/source 等字段的篡改矩阵；不能仅检查 JSON 格式或补一个可同样改写的标记字段。

### R03 [P1] 保护子目录不能阻止父目录整体清理

**位置：** [engine.rs:187–190](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L187)、[protected.rs:144–162](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/safety/protected.rs#L144)。

规则风险只对待清理的顶层路径 resolve；固定 ProtectedRootRegistry 并未纳入全部用户保护路径，也没有对待清理子树建立保护约束。于是子目录明确命中 UserProtected 规则，仍可能因父目录作为 Kondo 构建产物等整体进入清理而被一并清理。

- **证据：** `C:\review\target\credentials` 命中 UserProtected，父 `C:\review\target` 作为 RegenerableLocal 项，正常规划后仍到达假删除端口；没有篡改 Plan。
- **规格：** INV-002；若子目录存放 credential/authentication，还关联 INV-007。
- **修复验收：** 保护应向待删除的祖先生效：目标包含任一保护区域就拒绝整体清理，或拆分为独立验证的安全子项。覆盖“只选择父项”“子项未被扫描展示”“后来新增保护规则”“外部命令覆盖保护子项”。

### R04 [P1] ScanItemId 跨扫描复用，旧选择会指向新一轮的其他对象

**位置：** [scan_ctx.rs:158–166](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_ctx.rs#L158)、[scan_ctx.rs:219–224](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_ctx.rs#L219)、[scanStore.ts:56–68](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L56)、[selectionStore.ts](D:/Code/agent/opencode/dev-cleaner/src/stores/selectionStore.ts)、[plan.rs:28–64](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/plan.rs#L28)。

每个 ScanContext 的计数器从 0 开始；CLI/Tauri 以数字 ID 对“最新一次扫描”查找。UI 重新扫描时清空 items，但不清空共享 selection。只要新扫描的顺序或发现集合变化，旧的 ID=1 就可能自动选中新对象；后端也无法识别旧 ID 属于上一轮扫描。

- **证据：** Rust probe 验证两次扫描都分配 ID=1，旧 ID 可以为另一条路径生成 Plan。对真实 TS store 的隔离测试进一步验证：重扫后新路径保留选中状态。
- **影响：** 在不篡改数据、不利用文件系统竞态的正常操作中就可能发生错误选中；用户的对象选择不再可靠。
- **修复验收：** 使用跨扫描唯一 ID，或强制 `(scan_generation, item_id)`；后端拒绝所有旧代 selection，前端重扫清 selection、旧 Plan 和关联分析状态。不能只修 UI，因为 CLI/IPC 也存在歧义。

### R05 [P1] 通过 identity 校验后仍按裸 Path 删除，保留最后一段 TOCTOU 窗口

**位置：** [validator.rs:332–447](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/safety/validator.rs#L332)、[engine.rs:238–243](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L238)、[port.rs:63–70](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/port.rs#L63)、[identity/mod.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs)、[cleanup/mod.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs)。

Validator 比较 identity 后还执行 ProcessGuard，然后返回 Allow。Engine 忽略 Allow 中刷新后的 snapshot，重新以路径调用 DeletePort；底层查询 identity 的句柄没有作为删除对象的能力凭证保留。检查通过的对象与最后实际按路径操作的对象没有不可替换的绑定。

- **证据：** 假 ProcessProbe 在 identity 检查之后将 identity 从 1 改成 999，Engine 仍调用假删除端口并记录成功。
- **与 R01 不同：** R01 在扫描和规划之间；本项在最终重新验证和删除之间，提前采集快照不能修好本项。
- **规格：** INV-005 及 SPEC §32 TOCTOU Replacement Test 的实质目标。
- **修复验收：** 设计绑定已验证对象及祖先约束的删除能力/句柄生命周期；针对回收站、直接删除、外部命令分别定义可兑现的保证。不应仅再加一次路径查询。复现证明的是对象绑定缺失，不代表已证明 Rust 标准库必然跟随 junction，也没有进行真实替换删除攻击。

### R06 [P1] Tool-native cleanup 没有绑定已验证的缓存根

**位置：** [uv.rs:30–53](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/uv.rs#L30)、[npm.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/npm.rs)、[pip.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/pip.rs)、[bun.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/bun.rs)、[shell/mod.rs:54–72](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/shell/mod.rs#L54)。

Provider 查询并验证缓存路径 A，但保存的动作是无路径绑定的 `uv cache clean`、`npm cache clean --force`、`pip cache purge`、`bun pm cache rm`。执行器按当前环境/工作目录启动工具，没有冻结与扫描时相同的缓存作用域。跨 CLI 进程执行、工具配置改变等情况下，可以形成“验证 A，工具自行解析并清理 B，日志却记录 A”。

- **证据：** 调用真实 uv Provider 的隔离测试确认，返回的 ScanItem.path 是指定缓存根，而命令只含 `cache clean`，没有该路径参数，也没有工作目录绑定。作用域漂移的端到端真实工具清理未运行。
- **规格：** INV-011、SPEC §22、§32 Cleanup DryRun Consistency Test。
- **修复验收：** 按工具明确作用域控制方式，绑定可信可执行文件与目标缓存根，执行前重新查询并对照；无法把工具行为限定到已验证目标的动作应拒绝或降级为手动操作。新增 scan=A、execute 环境/配置=B 的拒绝测试。

### R07 [P1] 工具报告的 Workspace Root 未被拒绝

**位置：** [common.rs:66–112](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/common.rs#L66)、[CLI support.rs:80–84](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/support.rs#L80)、[Tauri support.rs:65–69](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/support.rs#L65)。

`verify_tool_path` 只检查盘符根与一组系统环境目录，没有检查 `ctx.workspace_roots`；异常位置只是 warning。CLI/Tauri 构造 ProtectedRootRegistry 时额外保护根传空数组，也没有在这里补上工作区根拒绝。

- **证据：** context 的工作区根是 `D:\review\workspace`，工具也报告这个根，函数返回 Accept。
- **规格：** SPEC §11 明确要求拒绝 Tool-reported Workspace Root。它不是普通“可告警后继续”的自定义缓存位置。
- **修复验收：** 工作区根及包含它的祖先必须拒绝，安全子目录再独立判定；实际执行复验也需有相同边界。覆盖多个 workspace roots、大小写/规范化及工具路径配置变化。

### R08 [P1] 原发现规则不再匹配时，旧计划反而被放行

**位置：** [engine.rs:174–190](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L174)、[planner.rs:266–270](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L266)。

`risk_rederivation_mismatch()` 对 `resolve(...) == None` 用 `?` 返回“无 mismatch”。它没有区分“本来就是 Provider 发现、应验证 Provider 证据”与“原先来自某条规则，但规则删除或前置 marker 失效”。snapshot 中 rule_id 没有用于保持发现依据有效；provider_id 在规划时传 None。

- **证据：** probe 先确认发现规则命中，ScanItem 携带该 RuleId，生成的快照也包含 RuleId；执行时换为不再匹配的非空规则集，Engine 仍调用假删除端口。
- **规格：** SPEC §15 的“仍匹配规则、Provider 一致、Risk 一致”要求。
- **修复验收：** 保存发现来源与版本/证据，分别对规则类、Kondo 类和 tool-reported 类重新验证；原依据失效时拒绝旧 Plan 并要求重扫，而不是 no match = allow。规则文件正常更新也应有回归测试。

### R09 [P2] 正式 `clean --safe` 仍接到 fixture 数据

**位置：** [clean_cmd.rs:33–49](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/clean_cmd.rs#L33)、[fixtures.rs:43](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/fixtures.rs#L43)。

safe_shortcut 直接调用 `fixture_scan_items()`，没有读取真实 latest scan，也没有执行真实扫描。候选是固定的 demo 用户缓存和 demo 项目目录。普通机器上无法清理实际扫描结果；如果硬编码路径恰好存在，还会尝试处理未通过真实扫描获得的目标。

- **证据：** 生产 CLI 调用链静态确认；本轮没有运行真实 `clean --safe`。
- **规格：** PLAN Phase 6、Phase 10 的正式 CLI 链路。
- **修复验收：** 明确 safe shortcut 的真实扫描/快照语义，只从该结果选择允许风险项并生成 Plan；fixture 只能存在于显式 demo 或测试入口。无 scan 的行为和 scope 都应可预测并有 CLI 集成测试。

### R10 [P2] Tauri 扫描事件与权威状态的发布顺序存在竞态

**位置：** [scan.rs:117–152](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/scan.rs#L117)、[scan.rs:360–391](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/scan.rs#L360)、[events.ts:47–64](D:/Code/agent/opencode/dev-cleaner/src/adapters/events.ts#L47)、[scanStore.ts:87–102](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L87)。

存在三处相关生命周期问题：

1. 后端先 emit Done，之后才保存 snapshot，再由 scan_job 调用 model.finish_scan。前端 Done 回调立即 get_scan_results，而该接口读取 model.latest，可能拿到旧扫描/尚无结果。
2. subscribeScan 用 `void wire()` 异步注册监听，没有 ready 屏障；前端还要等 scan 命令返回才设置 scanId。快速任务可能在监听完成或 ID 就绪前结束，Done 被漏掉/过滤，UI 一直 scanning。
3. scan_job 错误分支只 eprintln，未见同等的失败终态清理与前端通知；保存结果失败时还可能留下 active scan，影响下一次扫描。

- **证据：** 真实 TS scanStore + 假 I/O adapter 的两项测试，分别复现“Done 早于命令响应后一直 scanning”和“Done 先拉旧 snapshot，后端发布新结果后 UI 仍停在旧结果”。Rust 发布顺序为静态确认；未将 store 测试冒充 GUI/Tauri 端到端测试。第 3 项仅静态确认。
- **修复验收：** 先成功持久化并发布相应 generation 的权威结果，再发终态事件；订阅必须可 await，scan ID 应在事件可发出前建立或缓冲早到事件；成功/取消/失败都必须收束 active 状态。增加空扫描、首次启动、磁盘写失败和快速重扫测试。

### R11 [P2] Rules 页面展示的是演示注册表和演示验证结果

**位置：** [RulesPage.tsx:1–13](D:/Code/agent/opencode/dev-cleaner/src/pages/RulesPage.tsx#L1)、[RulesPage.tsx:38–46](D:/Code/agent/opencode/dev-cleaner/src/pages/RulesPage.tsx#L38)、[rules.ts:3–26](D:/Code/agent/opencode/dev-cleaner/src/data/rules.ts#L3)。

RulesPage 不读取后端实际加载的规则，而导入硬编码 `RULES` 与 `ruleValidation()`。数据注释明确 numeric ids 对齐 demo/mock；页面却显示“structure and path bounds checked at load time / validated”。用户规则、加载错误与真实 evidence→rule 关系不能通过该页面可靠检查。

- **证据：** 页面和数据模块静态确认。不是反对开发期有 mock，而是正式 Tauri 页面未区分并仍展示安全验证成功状态。
- **规格：** SPEC §27、PLAN Phase 12 的规则可解释/可查看能力。
- **修复验收：** 通过只读后端 API 展示实际合并后的规则集、来源、ID、校验问题及加载版本；mock 数据只在明确标注的演示模式使用。注入无效规则后 UI 不得仍显示全绿 validated。

## 3. 文档与实际阶段完成度

### 3.1 文档本身需要澄清/同步

1. **验收范围未对齐。** AGENTS 写“Phase 0–6 当前生效范围”，PLAN 列到 Phase 17 且仍为 Draft，工作区已出现大量后续阶段代码。应明确本次交付是 M1、CLI MVP 还是桌面完整发布，不能用“全部完成”混淆代码存在与阶段验收。第二批缓存 Provider 等后续缺项，不应反算为 Phase 0–6 的违约。
2. **没有可追溯的验收矩阵。** 为每个 Phase / INV 增补“实现入口、验收测试、执行环境、结果、未覆盖条件、阻断问题”。尤其需要区分测试函数通过与真实 NTFS/UNC/mount-point 条件具备。
3. **安全承诺需细化为可执行契约。** 明确 scan 快照何时产生和过期、跨扫描 ID 的命名空间、Plan 持久化信任边界、删除能力绑定、子树保护语义，以及外部工具的目标作用域。这是落实已有安全要求，不是放宽不变量来适配现状。
4. **发布状态缺少依据。** 增补当前交付版本、已知限制与禁止真实清理的状态，完成 Git 基线后再建立安全修复和验收记录。

### 3.2 阶段评估（不以文件数量估算完成百分比）

| 阶段 | 代码观察 | 审查判定 |
|---|---|---|
| Phase 0–4：Workspace / Domain / CLI Skeleton / Rule / Windows | 分层、类型、规则加载与多项 Windows 能力已实现；Core 不依赖 Tauri/React/WebView | 有实质实现，编译/现有测试通过；不等于所有平台边界已证明 |
| Phase 5–6：Safety / Planner / Engine | 模块齐全，有 dry-run、日志、风险确认、revalidation 与平台端口 | **不能验收**，R01–R05、R08、R09 等涉及当前生效安全链路 |
| Phase 7–9：Kondo / Cache / Agents | Kondo discovery adapter；npm/bun/pip/uv/cargo/NuGet 第一批；多种 Agent layout | 不是空壳；Cache 安全受 R06/R07 阻断。未见第二批 pnpm/yarn/rustup/Go/Gradle/Maven 独立实现，PLAN 第一批与第二批需分开验收 |
| Phase 10：CLI MVP | scan / plan / clean / journal 等真实路径存在 | `clean --safe` 仍是 fixture shortcut，不能认定完成 |
| Phase 11–12：Tauri / React 主页面 | 真实 IPC 与多页面 UI 已接入，同时保留浏览器 MockBackend | 不只是静态原型；R04/R10/R11 说明正式状态流与规则页未完成验收 |
| Phase 13–14：Unknown / Analyzer | Unknown、用户处置、metadata-only heuristic analyzer、默认关闭配置存在 | 基础路径已实现；当前 analyzer 是启发式实现，不等于已提供远程 LLM 服务。保护与来源验证仍受核心问题影响 |
| Phase 15：Hardening | 大量文件系统/安全测试已存在 | **不能因全绿判定完成**，本轮补充反例揭示关键漏测 |
| Phase 16：性能 | 有有界线程池、取消/进度等实现，另有显式 benchmark | 未运行规格规模的 50 万–100 万文件验收；性能达标未证明 |
| Phase 17：发布 | 有 lockfiles；Tauri 配置中 bundle.active=false | 未见完整打包签名、SBOM、audit、release hash 交付证据，不应宣称发布完成 |

根目录的 [tests/README.md](D:/Code/agent/opencode/dev-cleaner/tests/README.md) 仍是 placeholder，但各 crate 内确有集成测试，不能因此误报“没有集成测试”。同样，Provider 源码测试区的临时 fixture 清理不能被误报成生产 Provider 删除 authority。

## 4. 本轮验证与复现证据

### 4.1 现有项目验证

| 命令/检查 | 结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked --no-fail-fast`（TEMP/TMP 指向工作区） | **304 passed / 0 failed / 1 ignored** | [cargo-test-workspace-temp.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/cargo-test-workspace-temp.log) |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | **34 passed** | [tauri-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/tauri-test.log) |
| `npm run build` | TypeScript 检查及 Vite 构建通过 | [frontend-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/frontend-build.log) |
| `cargo fmt --all --check` | 通过 | [fmt.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/fmt.log) |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --check` | 通过 | [tauri-fmt.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/tauri-fmt.log) |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 | [clippy.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/clippy.log) |
| `cargo build --workspace --release --locked` | 通过（根 workspace，不含 Tauri 打包） | [release.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/release.log) |

初次默认临时目录执行出现用户目录祖先路径 AccessDenied；把 TEMP/TMP 改到工作区后全套通过。该初次失败作为环境限制记录，未单独计为产品缺陷。1 个 ignored 是显式性能 benchmark；部分 Windows 测试在管理员权限、UNC 或 mount point 不可用时直接 return，也会被测试框架计为 passed，因此上述计数不是所有高级文件系统场景均完成实测的保证。

### 4.2 独立 Rust 反例：9/9 成功复现

源码：[probes/src/lib.rs](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/probes/src/lib.rs)

最终日志：[probes-rerun.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/probes-rerun.log)

| 测试名 | 关联 |
|---|---|
| reproduces_replacement_between_scan_and_plan_is_accepted | R01 |
| reproduces_persisted_command_tamper_reaches_port | R02 |
| reproduces_review_confirmation_field_can_be_downgraded | R02 |
| reproduces_protected_descendant_does_not_block_parent_cleanup | R03 |
| reproduces_different_scans_reuse_same_item_id | R04 |
| reproduces_replacement_after_identity_check_before_delete | R05 |
| reproduces_uv_command_omits_validated_cache_root | R06 |
| reproduces_tool_reported_workspace_root_accepted | R07 |
| reproduces_missing_detection_rule_does_not_invalidate_plan | R08 |

这些测试使用假 PathProbe/ProcessProbe/只记录调用的 DeletePort，真正调用当前产品的 planner/engine/store/provider 代码。只写隔离的 fixture 与 Plan 文件，**不进行真实缓存删除，不运行外部清理命令**。测试断言的是“缺陷存在”，所以 9 passed 表示成功复现反例，而不是安全通过。修复后应将其转换成“拒绝/不调用端口”的正式回归断言。

### 4.3 实际前端 store 反例：3/3 成功复现

脚本：[store-probes.mjs](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/store-probes.mjs)

日志：[store-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260907/store-probes.log)

使用本地 esbuild 编译真实 scanStore / selectionStore，只替换 I/O adapters：

1. 重扫后旧 selection 自动命中新路径（R04）。
2. Done 在 scan 响应之前发生，状态一直 scanning（R10）。
3. Done 在权威 snapshot 发布之前发生，UI 最终显示上一轮结果（R10）。

这是 store 级受控时序测试，不是完整桌面 E2E。

### 4.4 安全复跑方式

以下命令仅执行本轮隔离反例；工作目录使用仓库根：

```powershell
Set-Location -LiteralPath 'D:\Code\agent\opencode\dev-cleaner'
cargo test --manifest-path 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907\probes\Cargo.toml' --offline -- --nocapture
node 'D:\Code\agent\opencode\dev-cleaner\target\review-20260907\store-probes.mjs'
```

### 4.5 未完成的验证，不作通过承诺

- 未执行对真实用户缓存、真实项目目录的破坏性清理。
- 未进行完整 GUI/Tauri E2E、安装/升级/卸载、签名与发布产物验证。
- 未运行 cargo audit / npm audit、Tauri 单独 release/clippy、安全规模性能 benchmark。
- 物理路径别名/短文件名、特殊 volume alias、Kondo root reparse 行为及子孙进程超时边界未形成完整复现；不把这些尚未证实的猜测计入上述 11 项。
- 未对依赖 clone 进行逐行许可证来源审计；本轮未编辑依赖 clone，也未复制 GPL 实现。

## 5. 建议整改顺序

1. **先冻结真实清理发布，修安全授权链：** R01/R02/R03/R04/R05/R08。将扫描身份、来源证据、选择命名空间、计划可信性和删除对象绑定作为一组设计问题处理，不只在当前函数加条件。
2. **封闭工具作用域：** R06/R07。逐个 Provider 证明查询、预览、验证、执行和日志指向同一范围。
3. **补齐正式使用路径：** R09/R10/R11，清理 demo 接线并统一扫描生命周期。
4. **建立能阻断发布的回归：** 将本轮反例转换为正式安全测试，补真实 Windows 条件与桌面 E2E，再更新 Phase/INV 验收矩阵及发布材料。

**最终判断：有可继续演进的实现基础；但关键安全验收尚未成立，当前不应签署“开发全部完成”或“可安全发布”。**
