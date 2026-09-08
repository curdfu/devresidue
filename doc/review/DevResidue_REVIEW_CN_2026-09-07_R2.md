# DevResidue 第二轮修复复核报告

- 复核日期：2026-09-07
- 工作区：`D:\Code\agent\opencode\dev-cleaner`
- 基线：当前工作区；Git 仍无 commit，产品源码为 untracked，不能用提交 diff 固定修复范围。本轮按第一轮问题逐项检查当前实现，并重新运行验证。
- 依据：[SPEC v0.3](D:/Code/agent/opencode/dev-cleaner/doc/spec/DevResidue_SPEC_CN_v0.3.md)、[PLAN v0.2](D:/Code/agent/opencode/dev-cleaner/doc/plan/DevResidue_PLAN_CN_v0.2.md)、[第一轮报告](D:/Code/agent/opencode/dev-cleaner/doc/review/DevResidue_REVIEW_CN_2026-09-07.md)。
- 范围：只复核，不修改产品实现；新增本报告及隔离验证材料。构建、已有 store 测试产生/更新了常规生成物。

## 1. 结论

**修复确实取得进展，但还不能关闭第一轮审查，也不建议批准真实数据清理或正式发布。**

不是简单重复第一轮结论：新的扫描快照、原始项目 lookup、精确子树保护、进程内 ID 分配、工具作用域复验、前后端终态处理和真实 Rules API 都已有实现，一些旧反例现在已经被正确拦截。但是边界条件及跨模块连接仍未闭合。

本轮归并为 **10 项剩余问题：7 项 P1、3 项 P2**。第一轮 11 项中，**R09、R11 可在本轮范围内关闭；其余 9 项均有有效修复，但仍有具体缺口**。R06 分成“uv 参数回归”和“dry-run 漏复验”两项说明。即使只按 Phase 0–6 验收，授权绑定、保护规则、TOCTOU、DryRun Consistency 等问题仍属于核心阻断项。

严重度仍沿用第一轮：P1 为应优先修复的安全契约/错误对象清理问题；P2 为功能、状态或预演一致性问题。涉及可写本地记录的反例，不等同于已证明远程攻击或权限提升。

## 2. 剩余问题（按严重度）

### F01 [P1] R02：授权查原始对象，验证和删除却用可改写的计划对象

**位置：** [engine.rs:343–352](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L343)、[engine.rs:397–416](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L397)、[engine.rs:264–269](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L264)、[engine.rs:503–511](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L503)。

`authorize_item()` 通过 ID 找到 `original`，以 `original.path` 检查保护规则及来源，却没有验证 `item.snapshot.path` 和扫描快照属于该对象。随后 SafetyValidator 和 DeletePort 都使用计划的 `item.snapshot.path`。因此“原始 A 的授权 + 被改写为 B 的快照”可以组合成有效执行。没有当前规则命中时，风险与确认要求也仍从计划的 `snapshot.risk` 推导，并未与原扫描项风险绑定。

**本轮独立证据：**

1. 经真实 PlanStore 保存、修改 JSON、重新加载后，把合法 A 的 snapshot 换成用户保护规则命中的 B，ID 和删除模式不变；假删除端口收到 **B**，结果为 `Success`。
2. 原扫描 identity=100，实际对象变为 200；仅把计划快照改为 200，就能绕过正常扫描快照已修好的 R01 检查。
3. 无规则命中的 Review 项，把计划风险改为 Safe、确认改为 None，默认无确认执行仍到达假端口。
4. 仅改计划的 mode 已会被拒绝，这是有效修复；但作为“可信来源”的扫描记录也只经过 JSON 反序列化。[CLI clean:153–161](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/clean_cmd.rs#L153) 与 [Tauri execute:129–135](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/plan.rs#L129) 都从可写的 `last-scan.json` 构造 lookup。独立 probe 同时修改扫描 action 和计划描述符，伪造命令仍到达假 ExternalCommand 端口，**未真正启动该命令**。把授权信任转移到另一份可编辑 JSON 还不够。

**验收要求：** 把 ID/扫描代次/对象身份/规范化目标/来源/风险/动作作为完整授权校验；不能让计划重新指定对象、降级风险或替换扫描基准。外部命令必须由可信内置策略重新构造或严格验证，持久化扫描记录不能仅凭可反序列化就成为命令授权。增加单字段及组合字段篡改矩阵。

### F02 [P1] R05：verified 删除仍是关闭句柄后按路径操作，未消除最后一次替换竞态

**位置：** [cleanup/mod.rs:83–90](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L83)、[cleanup/mod.rs:102–105](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L102)、[ffi.rs:129](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/ffi.rs#L129)。

`delete_tree_verified_impl()` / `recycle_verified_impl()` 都先打开并验证 identity，然后 `drop(handle)`，最后通过原路径删除/回收。替换发生在 drop 和路径重新打开之间时，实际删除对象仍可能不是验证对象。窗口短并不是对象绑定。

此外，共用打开函数指定 `FILE_SHARE_DELETE`，注释中“打开句柄会阻止 rename/replace”的前提不成立。

**证据边界：** 静态调用链直接显示 drop → path 操作。本轮还在隔离 Windows 目录中用与生产相同的打开标志，读取句柄 identity，在句柄仍打开时成功 rename 原目录并在原路径创建另一 identity 的目录。此测试证明该共享模式并不锁住路径对象；**未调用真实删除端口，也未声称已做端到端竞态误删演示**。微软 CreateFileW 文档的共享删除语义与测试一致。

**验收要求：** 使用真正绑定已验证对象的删除协议，或在无法保证时拒绝不安全模式；针对目标和祖先目录替换进行 Windows 集成验证。不能用“再查一次 identity”“微秒窗口”作为 TOCTOU 关闭证据；回收站模式也需要单独给出可验证的安全边界。

### F03 [P1] R04：generation 已保存，但未参与 ID 和计划授权；跨进程仍复用 ID

**位置：** [scan_ctx.rs:27](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_ctx.rs#L27)、[scan_store.rs:33–40](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_store.rs#L33)、[plan_cmd.rs:20–26](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/plan_cmd.rs#L20)、[domain/plan.rs](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/domain/plan.rs)。

全局 AtomicU64 只保证一个进程内不重复。每个新的 CLI 进程仍从 1 开始分配。ScanSnapshot 增加了 generation，但 CleanupPlan、选择参数和执行 lookup 没有同时绑定该 generation；CLI 读取 latest 后直接取 `snapshot.items`，丢掉代次。

**独立反例：** 启动两次真实 ScanContext 分配程序，两次首个 ID 都为 1；在同一隔离数据目录保存 generation 1 的 A，再保存 generation 2 的 B。使用旧选择 ID=1 生成计划，实际选中 B。无需修改 JSON 或制造竞态。

**已修部分：** 同进程 ID 不复用，前端 generation 变化清空选择的现有测试通过。

**验收要求：** 选择、计划、执行必须携带并强制比较同一个扫描代次，或使用跨进程不可混淆的 item 身份；重启、两个 CLI 进程及 CLI/Tauri 混用都必须拒绝旧选择，不能依靠 UI 单方清空选择。

### F04 [P1] R03：保护 glob 的字面根等于待删目录时，整树保护仍被绕过

**位置：** [loader.rs:183–199](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/rules/loader.rs#L183)、[planner.rs:315–323](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L315)、[engine.rs:354–361](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L354)。

`protected_areas()` 将保护 glob 转成字面 prefix root；但 planner/engine 的祖先保护判断都排除了 `area == target`，假定相等情况会由规则直接匹配处理。glob 不一定匹配它的字面根，所以假定不成立。

**独立反例：** 通过真实 YAML loader 成功加载 `user-protected` 规则 `C:/review/target/**/credentials`（0 个校验错误）。规则会保护 `target/nested/credentials`，却不直接命中 `target`；删除 `target` 时，两个保护检查都排除相等根，计划进入队列并到达假删除端口。

**已修部分：** 明确 exact 的受保护子路径能够同时在 planner 和 engine 被阻止。

**验收要求：** 按“删除树与保护范围是否相交”判断，而不是只检查严格包含；覆盖 glob 根相等、目标位于 glob 字面根之内、include/exclude 等情形。保守保护应真的形成安全上界。

### F05 [P1] R01：Tauri 指纹失败项目仍被持久化，无快照回退又将它重新授权

**位置：** [Tauri scan.rs:447–459](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/scan.rs#L447)、[planner.rs:356–366](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L356)、[ScanItem.scan_snapshot](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/domain/scan_item.rs#L56)。

Tauri 非 Kondo 分支遍历 `found.iter_mut()`，指纹失败后 `continue` 只跳过前端事件；循环后 `items.extend(found)` 仍将失败项目加入结果并持久化。该项没有扫描安全快照，但 planner 对所有 `scan_snapshot == None` 的项目都会重新 live capture，并没有把这个回退限定为不可执行的 fixture/demo。

所以临时权限/读取错误恢复后，或者该路径在扫描与规划之间被替换后，规划可以重新认可当前对象。兼容旧版本、缺少该字段的真实 `last-scan.json` 也有同样问题。

**证据：** Tauri 的失败保留由上述真实循环控制流确认；独立 probe 加载缺失 `scan_snapshot` 字段的旧记录，扫描基准 100 后当前变为 200，planner 保存 200 并让 engine 到达假端口。没有改写计划。与之相对，**携带正常扫描快照的项目**在同样替换下确实被拒绝，此修复已验证。

**验收要求：** 失败项不能因未发事件就被视为已排除；保证最终结果和持久化结果都排除/禁止执行无授权快照的真实项。旧真实扫描记录应要求重扫，不能默默采用规划时对象。补 Tauri 非 Kondo 指纹失败的持久化回归测试。

### F06 [P1] R08：真实规则重分类未记录结构化 RuleId，移除规则后旧 Safe 分类继续有效

**位置：** [scan_ctx.rs:352–372](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/scan_ctx.rs#L352)、[planner.rs:326–341](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L326)、[engine.rs:371–392](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L371)。

新失效检查仅覆盖 `SourceKind::Rule` 且有 RuleId 的项。但真实 `apply_rule_classification()` 改写 Provider 项的 risk/category 后，普通规则仍保留原 source，RuleId 只被写入说明字符串，没有调用 `.with_rule(...)` 设置结构化字段；只有 Protected 分类才切换到 Rule source。

**独立反例：** 用真实分类函数把 AgentProvider 的 Review 项通过用户规则分类成 Safe；随后移除该规则，使用不再命中的非空规则集。planner 与 engine 均未发现授权依据消失，无确认计划仍到达假删除端口。作为对照，人造 `SourceKind::Rule + RuleId` 的原反例现在会被正确拒绝。

**验收要求：** 同时保存发现来源和分类来源、结构化 ID/版本；凡最终授权依赖规则，都必须重验该规则，不能只靠单一 source 枚举分支。缺少原依据时重扫/重新分类，不能 no-match 即继续沿用旧 Safe。

### F07 [P1] R07：缓存专用扫描没有注入工作区根，保护逻辑只在部分入口生效

**位置：** [CLI scan.rs:94–110](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/scan.rs#L94)、[Tauri scan.rs:283–292](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/scan.rs#L283)、[common.rs:104–114](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/common.rs#L104)、[CLI support.rs:93–97](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/support.rs#L93)。

`verify_tool_path()` 新增的 workspace root/ancestor 拒绝本身有效，但需要 `ctx.workspace_roots` 非空。CLI 只有 `run_kondo` 时才解析工作区根，所以单独 `scan --dev-cache` 不会注入 `DEVRESIDUE_WORKSPACE_ROOTS` 或默认候选根；Tauri `ScanScope::DevCache` 明确使用 `RootsMode::None`。执行 SafetyValidator 也以空额外 roots 构造，不能在后续补拦。

**证据：** 入口连线如上；独立 context probe 设置有效的 `DEVRESIDUE_WORKSPACE_ROOTS`、保持与缓存专用入口相同的空 roots，工具报告该工作区根时得到 Accept。对照测试显式注入 roots 后，根和祖先都会被拒绝。

**验收要求：** 将“需要保护哪些根”与“是否运行 Kondo 发现”解耦；所有扫描 scope 及执行阶段都必须获得完整保护根。增加 CLI `--dev-cache` 和 Tauri DevCache 的配置根/默认根集成测试。

### F08 [P2] R06：uv 修复把缓存目录当作包名位置参数传入

**位置：** [uv.rs:46–55](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/uv.rs#L46)、[uv.rs:80–87](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-providers/src/dev_cache/uv.rs#L80)。

当前生成 `uv cache clean <绝对缓存路径>`，注释称位置参数可指定缓存目录。官方 CLI 定义是 `uv cache clean [OPTIONS] [PACKAGE]...`；位置参数是包名，缓存目录应通过 `--cache-dir` 等受支持方式指定。当前 Windows 绝对路径被放在包名位置，不能实现所声明的缓存清理。

**证据：** 真实 uv Provider 在隔离目录生成的描述符已断言为上述 argv；官方 CLI 文档已抓取核对。本机未找到 uv，**没有执行实际 uv 清理**，因此不伪造具体 exit code 或 stderr。

**验收要求：** 使用 uv 官方支持的缓存目录选项，并在隔离缓存里做真实 CLI 合约测试。仅测试“args 中含有路径”会把错误语法固化成绿灯。

### F09 [P2] R06：dry-run 跳过新增的工具作用域复验，与真实执行不一致

**位置：** [engine.rs:274–283](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L274)、[engine.rs:513–515](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L513)。

作用域查询被放在真实执行的 `execute_with()` 中。dry-run 在 validator Allow 后直接返回 WouldExecute，因此没有执行相同的 scope 验证。

**独立反例：** 冻结缓存 A、查询端口始终返回 B，其他条件完全相同：dry-run=`WouldExecute`，真实模式=`Failed(tool-scope-drift)`，两次都没有执行外部命令。不是两次运行间环境变化，而是同一个假查询的确定性差异。

**验收要求：** 将非破坏性的 scope 验证纳入共用 preflight，再按 dry-run 区分是否执行操作；覆盖工具消失、查询失败、scope 变化及相同 scope。对应 SPEC §32 的 Cleanup DryRun Consistency 发布阻断测试。

### F10 [P2] R10：第二次快速扫描仍丢失提前到达的 Done/Error

**位置：** [scanStore.ts:91–102](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L91)、[scanStore.ts:229–245](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L229)。

`startScan()` 进入新扫描时没有清空或隔离旧 `scanId`。早到 Done 的 pending 分支只接受 `known === null`，而正常连续第二次扫描保留旧 ID；新 Done/Error 在命令响应返回之前到达时被误判为别的扫描丢弃。响应只更新 ID，不再有终态事件，界面留在 scanning。

**独立复现：** 从 `phase=done, scanId=1` 开始第二次扫描，backend.scan 在返回 ID=2 前发 Done 或 Error。两个真实 store 的隔离场景最终都停在 `phase=scanning, scanId=2`。现有 storeProbes 的快速完成场景先调用 reset，因此只测到了无旧 ID 的情况。

**已修部分：** 订阅 ready、首次快速完成、后端先发布权威 snapshot 再发送 Done、失败终态等路径已有改进，现有五项 store 验证通过。

**验收要求：** 新扫描应有独立的 pending/epoch 状态，将早到事件与本次命令响应关联，拒绝旧事件的同时不丢本次事件；增加“不 reset 的连续两次扫描”及早到 Error 回归。

## 3. 第一轮 11 项逐项状态

| 第一轮 | 本轮判定 | 已证实的改进 | 尚未关闭原因 |
|---|---|---|---|
| R01 扫描快照 | 部分修复 | 正常 scan snapshot 被 plan 沿用；替换被拒绝 | F05：Tauri 失败项保留、缺快照真实项回退重新 capture；另有 F01 的篡改绕过 |
| R02 计划授权 | 部分修复 | 仅改 mode/command 等旧篡改会被 lookup 检查拦截 | F01：目标/identity/risk 未绑定，扫描 JSON 本身不是可信命令来源 |
| R03 保护子树 | 部分修复 | exact 保护子路径在 plan/execute 均拒绝 | F04：glob 字面根等于 target 时漏拦 |
| R04 ID 复用 | 部分修复 | 同进程原子 ID、前端代次变化清空选择 | F03：跨进程 ID 复用，generation 未贯穿授权 |
| R05 最终竞态 | 部分修复 | 增加 verified port，端口前 identity 不符会拒绝 | F02：关闭句柄后仍按路径删除，share-delete 不阻止 rename |
| R06 工具范围 | 部分修复，并引入回归 | 实际执行前查询 scope，漂移时拒绝 | F08 uv argv 错误；F09 dry-run 不执行相同检查 |
| R07 工具返回工作区根 | 部分修复 | 注入 roots 时正确拒绝根/祖先 | F07：缓存专用入口没有注入这些 roots |
| R08 发现规则失效 | 部分修复 | Rule source + 结构化 RuleId 的旧反例已拒绝 | F06：真实 Provider 重分类缺失结构化分类来源 |
| R09 clean --safe fixture | **本轮可关闭** | 加载 latest real scan，拒绝 fixture/无扫描，保留 partial gate，选择 Safe/RegenerableLocal | 不代表共用 cleanup engine 的其他问题已解决 |
| R10 扫描生命周期 | 部分修复 | 后端终态顺序、首扫 ready/pending/error 修复有测试 | F10：连续第二次快速扫描仍会丢终态 |
| R11 Rules 假数据 | **本轮可关闭** | 页面使用真实 getRules/validateRules；后端合并 builtin/user 并输出 issues；mock 有标识 | 本轮未做打包后的交互式 GUI 人工验收 |

R09 依据：[clean_cmd.rs:76–112](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/clean_cmd.rs#L76) 及对应 workspace 测试。R11 依据：[RulesPage.tsx:21–31](D:/Code/agent/opencode/dev-cleaner/src/pages/RulesPage.tsx#L21)、[rules.rs](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/rules.rs) 及两项 Tauri rules 测试。

## 4. 实际执行的验证

日志目录：[第二轮验证材料](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2)。测试 TEMP/TMP 指向该目录下的 tmp。

| 验证 | 实际结果 | 日志 |
|---|---|---|
| cargo test --workspace --locked --no-fail-fast | **332 passed，0 failed，1 ignored**（显式 benchmark） | cargo-test.log |
| cargo test --manifest-path src-tauri/Cargo.toml --locked | **41 passed** | tauri-test.log |
| npm run build | 通过 | frontend-build.log |
| node src/testing/storeProbes.mjs | 现有 **5/5** 修复验证通过 | store-tests.log |
| cargo fmt --all -- --check | 通过 | fmt.log |
| cargo clippy --workspace --all-targets --locked -- -D warnings | 通过 | clippy.log |
| Tauri 独立 manifest 的 fmt / all-targets clippy -D warnings | 均通过 | tauri-fmt.log / tauri-clippy.log |
| cargo build --release --workspace --locked | 通过 | release-build.log |
| 第二轮独立 Rust probes | **17 个断言场景通过**，含 5 个修复确认及 12 个缺口/行为证据场景 | probe-test.log |
| 第二轮真实 store 隔离反例 | **2/2 缺陷复现**：连续重扫 early Done、early Error | store-repro.log |

**重要：独立 probes 的 passed 不表示产品通过安全验收。** `fixed_*` 测试断言修复后拒绝；`reproduces_*` 等测试则故意断言目前仍发生的错误放行/错误状态。uv 场景验证生成描述符，Windows 场景验证同标志句柄的 rename 行为，未伪装成真实清理成功。

独立材料：

- [Rust 反例与修复确认源码](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/probes/src/lib.rs)
- [跨进程 ID 分配程序](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/probes/src/main.rs)
- [真实 store 的第二轮反例](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/store-probes.mjs)
- [Rust 验证日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/probe-test.log)
- [store 复现日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/store-repro.log)

复跑（从工作区根目录；只操作隔离 fixture/假删除端口）：

```powershell
$env:TEMP = (Resolve-Path 'target/review-20260907-round2/tmp').Path
$env:TMP = $env:TEMP
cargo build --offline --manifest-path target/review-20260907-round2/probes/Cargo.toml
cargo test --offline --manifest-path target/review-20260907-round2/probes/Cargo.toml -- --nocapture
node target/review-20260907-round2/store-probes.mjs
```

独立 probe 第一次联网解析遇到本机 schannel 凭据错误，随后使用本机已缓存依赖 `--offline` 成功构建并运行；不是跳过编译或根据源码假定通过。

## 5. 外部合约核对与边界

仅对需要外部合约的两点核对官方资料，均于本轮通过 Node fetch 抓取，未关闭 TLS 验证：

- uv CLI 官方参考：`https://docs.astral.sh/uv/reference/cli/#uv-cache-clean`；[抓取副本](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/uv-cli-official.html)。位置参数定义为 PACKAGE，目录选项为 --cache-dir。
- Microsoft CreateFileW：`https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew`；[抓取副本](D:/Code/agent/opencode/dev-cleaner/target/review-20260907-round2/createfilew-official.html)。共享删除权限允许后续 delete-access 打开，相关权限涵盖 rename。

本轮没有对用户真实缓存/开发数据运行清理，没有调用伪造外部命令，没有修改依赖源码克隆。删除行为反例使用记录型假 DeletePort；唯一原生文件系统行为验证是在本工作区 target 下创建、移动空的唯一 fixture，并保留审计材料。

未覆盖：真实 uv 清理、打包后的 Tauri GUI 人工操作、完整 Windows 删除/回收站竞态压力矩阵、长时间运行与所有 Provider 的全量行为。本文对 Tauri 指纹失败保留、专用 scope 根缺失的结论来自生产调用链；相邻 planner/context 行为有独立 probes，但未把它们描述为已运行整个 Tauri 端到端场景。

## 6. 下一轮建议验收顺序

1. **先修授权对象绑定和来源信任（F01）**，否则单项安全修复仍能从计划字段绕过。
2. 修复真实扫描快照 fail-closed、跨进程 generation、保护范围交集、分类来源及全入口工作区根（F03–F07）。
3. 给 Windows 删除/回收站建立真实可验证的对象绑定协议与替换测试（F02），不要只改命名或增加一次路径检查。
4. 校正工具 argv，将 scope preflight 同时用于 dry-run 和执行；补连续重扫状态测试（F08–F10）。
5. 将本轮反例改成“必须拒绝/正确终态”的正式回归后，重新运行现有测试与 SPEC §32 阻断测试；绿灯条件要覆盖真实入口，而不只是单个 helper。

**最终判定：继续修复后再复核，不批准当前版本进入真实数据清理验收或正式发布。**
