# DevResidue R5 复核报告

日期：2026-09-08（Asia/Hong_Kong）  
审查对象：用户声明已修复的 R4 H01–H06，以及修复涉及的相邻执行路径。  
方式：当前源码核对 + 正式测试/构建门禁 + 独立隔离反例；未修改产品实现。

## 1. Findings（仍存在的问题）

### I01 [high] 回收站分支仍可回收替换对象并错误返回成功（R4-H03 未闭环）

**代码位置：** [cleanup/mod.rs:234–253](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L234)。

当前流程在 `drop(rebound)` 后把 staged 路径交给 shell；成功后仅检查 staged 路径是否消失，`NotFound` 直接返回 `Ok(())`。路径消失不能证明被回收的对象就是用户选中的对象。

确定性反例：

1. 验证、staging 的确操作了用户选中的对象 A。
2. 释放验证句柄后，另一个 actor 将 A 从 staged 名移走保存。
3. 在 staged 名创建另一个对象 B。
4. shell 成功回收 B，因此 staged 名消失。
5. 后置检查读到 `NotFound`，整个 verified recycle 返回 `Ok(())`，但 A 仍在别处。

本轮先用移动到隔离“模拟回收位置”的 callback 复现，再将步骤 4 换为当前平台代码的真实 `crate::recycle_bin::recycle(staged)`，调用真实 `IFileOperation`，结果仍为 **API 返回成功，A 保留，staged 上的 B 被移走**。所有目录和内容均为本轮专门创建的测试 fixture，没有对实际开发缓存发起清理，也没有检查或清空已有回收站内容。

新增产品测试 [cleanup/mod.rs:1027–1048](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L1027) 只“植入 replacement 后返回 Ok”，并未实际移走 replacement。因此 staged 仍存在，该测试只覆盖“shell 报成功但路径仍在”的情况，无法证明真实成功回收替换对象时也会拒绝。

**影响：** 用户授权的对象绑定仍然失效；可能回收未授权对象，并把错误对象操作记为成功。违反本次验收所要求的最终删除对象绑定，不能以“后置检查已增加”关闭 H03。

**修复要求：** 保证最终消费者操作的是已验证对象，而不是可被重新绑定的名称；不能仅在操作结束后增加另一次路径检查。如果当前 shell 接口/交接协议不能满足对象绑定合同，必须明确能力限制并 fail closed，不能宣称该窗口已经验证，也不能降级为永久删除。

**证据：**

- [真实 COM 反例日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-real-com-probe.log)
- [原样协议副本 + 模拟 shell 移动反例日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-probes.log)
- [隔离 probe 源码](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-probe/src/cleanup/mod.rs)

### I02 [high] `restore_no_clobber` 仍是 check-then-act，可覆盖竞态中新建的文件（R4-H02 部分修复）

**代码位置：** [cleanup/mod.rs:288–304](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L288)。

当前实现先 `symlink_metadata(original)`，看到 `NotFound` 后再执行 `std::fs::rename(staged, original)`。两次调用之间存在窗口，预检查不是原子 no-clobber。

确定性时序：

1. metadata 检查看到 original 不存在。
2. 另一个 actor 在 original 创建新文件 B。
3. 后续 rename 用 staged 文件 A 覆盖 B，恢复函数返回成功。

本轮在隔离文件上实际验证了该 rename 覆盖行为；又在**隔离源码副本**的“已经得到 NotFound、尚未 rename”处插入仅用于安排时序的 hook，调用当前 `restore_no_clobber` 分支，确认 B 被覆盖且返回 `Ok`。产品文件没有改动。

**证据边界：** 这是显式控制交错顺序的算法/分支反例，不是宣称在未经插桩的并发压力测试中测得了自然命中概率。调度 hook 只模拟另一个 actor 在现存窗口创建目的文件，不替换 rename、不修改 metadata 结果，也不改变恢复分支的判断。

**已经修好的部分：** 恢复检查开始前 original 已占用时会拒绝覆盖；恢复失败会返回 staged 位置，不再静默丢弃错误。这两点通过当前产品测试验证。但它们不等于恢复提交本身无覆盖。

**影响：** 在 shell 失败后的恢复路径上，可能丢失一个未获清理授权的新文件，仍与 no-clobber 的恢复合同冲突。

**修复要求：** 使用具备原子“不替换现有目标”语义的恢复提交，并将恢复源重新绑定到预期身份；不要用 exists/metadata 预检查来模拟这一原子语义。失败时应保留对象并返回可信的可恢复位置。

**证据：**

- [基础交错反例](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-probes.log)
- [实际恢复分支 + 调度接缝日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-seam-probes.log)
- [加入调度接缝前的源码副本（仅追加独立 tests）](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/cleanup-pristine-plus-probes.rs)

### I03 [medium] 新 DirectDelete 实现无条件枚举目录，普通文件被改名后清理失败

**代码位置：** [cleanup/mod.rs:114–115](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L114)，[cleanup/mod.rs:444–450](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L444)。

H03 的直接删除修复将最后的 root 删除改为通过句柄提交，这改善了目录根对象绑定。但新实现不区分文件/目录，无条件调用 `remove_tree_contents(&staged)`，其内部直接 `read_dir(path)`。

对普通文件调用 `WindowsDeletePort.delete_tree_verified` 时：

1. 身份检查成功；
2. 文件成功改名到 staged 名；
3. `read_dir(staged_file)` 返回 `os error 267`（目录名称无效）；
4. 没有执行句柄删除，原路径消失，文件内容留在 staged 文件中。

本轮不仅在源码副本测试，还由独立 Rust 程序直接链接**未修改的生产 platform crate**调用其公开 verified-delete 接口，确认相同行为。

**影响与范围：** 已声明支持文件/目录的删除 port 在普通文件路径上发生功能回归，并在失败前改变文件名。该问题针对 DirectDelete/verified-delete 能力；不意味着当前默认使用 RecycleBin 的 provider 文件项也走此分支，不能据此声称所有普通文件清理都失败。

**修复要求：** 基于绑定对象的种类分别处理：普通文件不枚举子项，普通目录才清空内容，reparse 必须遵守不跟随策略；最后仍保持同一已验证句柄提交，不退回 drop 后按路径删除。

**证据：** [直接调用生产 crate 的日志](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/rust-cli-probes.log)、[独立程序源码](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/probes/src/main.rs)。

## 2. 结论与 R4 关闭矩阵

**本轮结论：不能接受“R4 全部修复完成”；暂不放行。**

R4 六项中，**4 项可关闭，2 项部分修复但未闭环**；另确认 **1 项 medium 相邻回归**。本轮仍有 2 项 high 对象绑定/恢复安全问题；正式测试、编译、lint 全绿不能替代这些反例的闭环。

| R4 项 | R5 状态 | 验证依据 |
|---|---|---|
| H01 staging 名称构造 | **关闭（本轮覆盖范围内）** | 缓冲区增加 NUL 空间，并验证 rename 后路径身份；平台正式测试通过，另固定运行 10 组 × 100 次 staging/删除检查，全部通过。未复现乱码 staged 名。 |
| H02 回收失败恢复 | **部分修复，不能关闭** | 预先占位场景和失败位置报告已修；提交仍非原子 no-clobber，见 I02。 |
| H03 最终对象绑定 | **部分修复，不能关闭** | DirectDelete 目录根改为持有句柄至提交；RecycleBin 仍可错回收并报成功，见 I01；DirectDelete 普通文件另有 I03。 |
| H04 selection generation | **关闭** | CLI 缺失/过时代次均拒绝且不生成计划，当前代次正常；Tauri 生产命令 None 分支明确拒绝。 |
| H05 当前 workspace roots | **关闭** | 保留 scan-time snapshot 的同时，计划阶段另查当前 registry；真实 release CLI 的根项被排除，混合 selection 中外部正常项仍能计划，旧 plan 的 dry-run 也跳过新保护根。 |
| H06 PendingTerminal 覆盖 | **关闭** | 改为按 scanId 保存；产品 12/12 probes、R4 原始覆盖反例、额外 4 组 Done/Error 交错均通过。 |

关闭项关键实现：

- [identity/mod.rs:230](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs#L230)
- [CLI plan_cmd.rs:32](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-cli/src/plan_cmd.rs#L32)
- [Tauri plan.rs:49](D:/Code/agent/opencode/dev-cleaner/src-tauri/src/plan.rs#L49)
- [planner.rs:258](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/planner.rs#L258)
- [scanStore.ts:325](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L325)、[scanStore.ts:363](D:/Code/agent/opencode/dev-cleaner/src/stores/scanStore.ts#L363)

## 3. 本轮实际执行的正式门禁

以下均为 **2026-09-08 本轮运行**，不是沿用 R4 的成功日志。

| 命令/检查 | 实际结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked --no-fail-fast` | exit 0；汇总 378 passed、0 failed、2 ignored | [workspace-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/workspace-test.log) |
| `cargo test -p devresidue-platform-windows --lib --locked -- --nocapture` | exit 0；41 passed | [platform-unit.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-unit.log) |
| `cargo test -p devresidue-cli --locked` | exit 0；23 passed | [cli-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/cli-test.log) |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | exit 0；45 passed | [tauri-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/tauri-test.log) |
| `node src/testing/storeProbes.mjs` | exit 0；12/12 | [store-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/store-test.log) |
| `npm run build` | exit 0；TypeScript 检查及 Vite 生产构建通过 | [frontend-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/frontend-build.log) |
| `cargo fmt --all -- --check` | exit 0 | [fmt-check.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/fmt-check.log) |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | exit 0 | [tauri-fmt-check.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/tauri-fmt-check.log) |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | exit 0 | [workspace-clippy.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/workspace-clippy.log) |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features --locked -- -D warnings` | exit 0 | [tauri-clippy.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/tauri-clippy.log) |
| `cargo build --workspace --release --locked` | exit 0 | [workspace-release-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/workspace-release-build.log) |
| `cargo build --manifest-path src-tauri/Cargo.toml --release --locked` | exit 0 | [tauri-release-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/tauri-release-build.log) |

注意：平台 41 项和 CLI 23 项已经包含在 workspace 378 项中，不应再相加当作不同用例。两项 ignored 分别是 allocator child helper 和显式 benchmark；本轮未将日志中的“子进程 running 0 tests”计作额外跨进程覆盖。

## 4. 独立验收与反例

| 检查 | 结果与含义 | 证据 |
|---|---|---|
| H01 固定次数重复 | 10 次调用，每次实际运行 1 个包含 100 次循环的正式测试；共 1,000 次检查，无失败 | [h01-stress-corrected.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/h01-stress-corrected.log) |
| H04 release CLI | 旧 generation 拒绝；缺失 generation 拒绝；匹配 generation 只计划当前选中对象 | [rust-cli-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/rust-cli-probes.log) |
| H05 release CLI | 新保护根单独 selection 被拒；混合 selection 只保留非保护项；既有 plan 的 dry-run 输出 protected-root-exact、Would Skip | [rust-cli-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/rust-cli-probes.log) |
| H06 原始 R4 反例 | `new Done42 → old Done41 → response42` 最终 done / scanId 42 / generation 8 | [store-terminal-r4.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/store-terminal-r4.log) |
| H06 相邻交错 | 新 Done 后旧 Error、同 ID Done→Error、同 ID Error→Done、多无关 ID 终态均符合期望；4/4 | [store-interleavings.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/store-interleavings.log) |
| I01/H03 模拟 shell 移动 | 复现错对象移动仍返回成功 | [platform-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-probes.log) |
| I01/H03 真实 COM | 复现真实 IFileOperation 操作 replacement 后，verified recycle 错误返回成功 | [platform-real-com-probe.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-real-com-probe.log) |
| I02/H02 调度接缝 | 控制 metadata 与 rename 之间交错，复现未选中文件被覆盖 | [platform-seam-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-seam-probes.log) |
| I03 生产 crate 调用 | 普通文件在 staged 后因 `read_dir` 失败，内容残留而原名消失 | [rust-cli-probes.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/rust-cli-probes.log) |

**`repro_*` 的 passed 表示“成功证明缺陷发生”，不是“安全验收通过”。** 正式门禁与这些反例必须分别解读。

H01 首次重复命令将短测试名配合 `--exact` 使用，实际选择了 0 tests，日志保留在 [h01-stress.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/h01-stress.log)，**没有计入任何覆盖或稳定性结论**。发现后使用完整模块限定名重新固定运行 10 次，并检查每次输出确实为 `1 passed; 0 failed`。这不是“失败后重试到绿”，前一组没有执行目标用例。

## 5. 审查边界、可复现性与未覆盖项

### 5.1 审查基线

依据：

- [R4 问题单](D:/Code/agent/opencode/dev-cleaner/doc/review/DevResidue_REVIEW_CN_2026-09-07_R4.md)
- [SPEC v0.3，尤其 §31–32](D:/Code/agent/opencode/dev-cleaner/doc/spec/DevResidue_SPEC_CN_v0.3.md)
- [PLAN v0.2](D:/Code/agent/opencode/dev-cleaner/doc/plan/DevResidue_PLAN_CN_v0.2.md)

仓库仍没有有效 HEAD（`git rev-parse --verify HEAD` 失败），源码为 untracked。因此本轮是基于用户明确指定的 **R4 行为基线**进行当前实现验收，不是 Git diff 审查；没有虚构提交基线、本地 tracked diff 或逐提交归因。不能从本报告推导出整个仓库、所有 Phase 的全量需求均已重新审核通过。

### 5.2 隔离与源码完整性

- 日志、probe 包和 disposable fixtures 位于 `target/review-20260908-round5/`；测试进程 TEMP/TMP 指向该目录的 `tmp/`。
- CLI 独立 probe 只执行 `plan` 和 `clean --dry-run`，数据目录显式隔离。另有一次公开 platform 接口调用只操作本轮创建的普通文件。
- 平台正式测试及真实 COM 反例仅删除/回收测试创建的 fixture；没有实际缓存清理，也没有遍历/清空现有回收站。
- 没有修改产品 Rust/TypeScript 实现；没有编辑依赖源码克隆。运行现有 store probe 脚本会按脚本行为重建 `src/testing/store-probes.bundle.mjs`，正常 build 会更新 target/dist 生成物。
- I01 的函数体来自当前原样副本，只利用现有 injectable callback 控制释放句柄之后的替换时序；真实 COM 版本调用当前 `recycle_bin` 实现，不使用假的 COM 返回值。
- I02 唯一额外插桩在隔离副本的 restore NotFound→rename 交接处；加入前副本与加入后源码均保留，未把它冒充完全未经插桩的并发运行。
- 关键平台生产文件的前后 SHA-256 比较为 **0 项变化**：[before](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-production-hashes-before.json)、[after](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/platform-production-hashes-after.json)。[本轮主要审查文件 hashes](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round5/reviewed-source-hashes.json)用于定位当前源码状态。

### 5.3 未覆盖项

- 没有重新进行完整 Tauri GUI/打包安装/交互式端到端验收。Tauri 45 项测试和前端 store probes不等于真实 WebView 端到端测试。
- Tauri generation 测试使用复制 gate 逻辑的 test helper；本轮同时核对生产命令确实拒绝 None，但未声称测试通过直接覆盖了真实 IPC 路由。
- 没有将定向竞态反例的结果推广为自然并发命中概率；这是可实现交错下的正确性失败证明。
- 没有重跑 R4 独立的 8 个 writer 跨进程验收；本轮 workspace 中的现有 allocator 测试通过不用于替代该独立覆盖声明。
- H01 的 1,000 次检查仅证明本机、正式用例覆盖下的稳定性，不代表所有文件系统、网络盘、路径编码和长度组合均经过穷举。

## 6. 下一轮验收要求

1. **先关闭 I01/H03 和 I02/H02。** 最终对象绑定和恢复原子性必须在操作提交点成立，不能只靠修改注释或增加前后路径检查。
2. 将“替换对象确实被 shell 移走”的反例纳入正式 H03 回归用例，而不是让 mock 仅返回 Ok 且保留路径。
3. 恢复测试必须覆盖“检查后、提交前才出现 original”的交错，并断言 replacement 内容/身份不变。
4. 修复 I03，加入 untouched regular-file 的 verified DirectDelete 正向测试；同时保持目录、reparse、替换对象拒绝测试。
5. 保留并复跑已经关闭的 H01/H04/H05/H06 测试与正式门禁。

**最终判断：正式门禁全绿；R4 安全问题尚未全部闭环，暂不放行。**
