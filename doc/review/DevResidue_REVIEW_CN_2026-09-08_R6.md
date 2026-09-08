# DevResidue R6 修复与复核报告

- 日期：2026-09-08（Asia/Hong_Kong）
- 工作区：`D:\Code\agent\opencode\dev-cleaner`
- 复核范围：R5 的 I01、I02、I03，以及相关 CleanupEngine 分派、公开 Windows DeletePort 合同和回归测试。
- 基线说明：当前 Git 工作区尚无可用 tracked baseline，项目文件均显示为 untracked，因此本轮按当前全量源码复核，不能给出可靠的 commit/diff 基线。

## 1. 结论

**R5 的三项遗留问题均已关闭，修改范围内未发现新的 release blocker。**

- I01 已通过明确的 fail-closed 能力限制关闭：verified `RecycleBin` 在完成同句柄身份验证后返回 `UnboundDeleteRefused`，不 staging、不调用 Shell、不回收替换对象，也不偷偷降级为永久删除。
- I02 已通过删除整条 staged → shell → restore 协议关闭；`restore_no_clobber` 及相关 helper 已不存在，不再有检查后再覆盖恢复的竞态。
- I03 已修复：verified `DirectDelete` 从同一个已验证句柄判断对象类型，普通文件不再调用 `read_dir`，最终仍通过绑定句柄提交删除。

**放行判断：**就 R5 修复验收而言可以放行。需要明确记录一项产品能力限制：Windows verified `RecycleBin` 当前不可用；具有 `file_identity` 的 RecycleBin 计划会安全拒绝，而不是执行可恢复删除。普通、未绑定身份的 `recycle(path)` 能力仍存在，但 CleanupEngine 对有 identity 的项目不会回退到它。若产品发布标准要求“所有正常 identity-bearing 项均可进入回收站”，则该功能目标仍未实现；这不是剩余的错误对象删除漏洞，而是有意的安全限制。

## 2. R5 问题关闭情况

### I01 [closed] verified RecycleBin 错误对象回收

实现位置：

- [cleanup/mod.rs:143](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L143)
- [port.rs:123](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/port.rs#L123)
- [engine.rs:614](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-core/src/cleanup/engine.rs#L614)

当前实现先调用 `open_for_rebind`，在同一 pinning handle 上验证 NTFS identity；验证通过后直接返回结构化 `UnboundDeleteRefused`。整个分支不进行 staging、不调用 `IFileOperation`、不删除任何对象。

CleanupEngine 分派也已核对：有 identity 的 `RecycleBin` 只调用 `recycle_verified`，错误会作为本项目失败返回，不会自动调用普通 `recycle(path)`，也不会切换到 `DirectDelete`。

回归覆盖：

- 原对象 untouched 时明确拒绝，内容及 volume/file index 不变；
- 路径已被替换时先返回 `IdentityMismatch`，原对象与 replacement 均保留；
- 拒绝后父目录中不存在 `devresidue-staged-*` 残留。

### I02 [closed] restore_no_clobber check-then-act 覆盖竞态

不安全的回收恢复协议已全部移除：

- `recycle_verified_with`
- `restore_no_clobber`
- `handle_identity_matches`
- staged → shell → restore 及事后 staged-path 检查

源码检索结果为 `NO_MATCHES`。保留的句柄 rename 用于 verified `DirectDelete` staging，且 `FILE_RENAME_INFO.ReplaceIfExists` 为 false。新增测试在 destination 已存在时验证：rename 原子拒绝，source 内容保留，destination 内容和 NTFS identity 均不变。

实现及测试位置：

- [identity/mod.rs:189](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs#L189)
- [cleanup/mod.rs:737](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L737)

### I03 [closed] verified DirectDelete 普通文件 staged 后残留

`delete_tree_verified_impl` 现在在 staging 后调用 `handle_is_traversable_directory(&rebound)`：

- 真实普通目录：清理 descendants，再通过同一 handle 设置删除 disposition；
- 普通文件：跳过 `remove_tree_contents/read_dir`，直接通过同一 handle 删除；
- reparse directory：不遍历目标，遵守 INV-004。

实现位置：

- [cleanup/mod.rs:98](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/cleanup/mod.rs#L98)
- [identity/mod.rs:326](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs#L326)
- [identity/mod.rs:359](D:/Code/agent/opencode/dev-cleaner/crates/devresidue-platform-windows/src/identity/mod.rs#L359)

回归测试确认普通文件删除成功、原路径消失、父目录没有 staged residue；替换文件仍返回 `IdentityMismatch` 且 replacement 保留。

## 3. 修改文件

产品实现：

- `D:\Code\agent\opencode\dev-cleaner\crates\devresidue-platform-windows\src\cleanup\mod.rs`
- `D:\Code\agent\opencode\dev-cleaner\crates\devresidue-platform-windows\src\identity\mod.rs`
- `D:\Code\agent\opencode\dev-cleaner\crates\devresidue-core\src\cleanup\port.rs`

测试：

- `D:\Code\agent\opencode\dev-cleaner\crates\devresidue-platform-windows\tests\cleanup_adapter.rs`
- `D:\Code\agent\opencode\dev-cleaner\crates\devresidue-platform-windows\tests\verified_delete.rs`

## 4. 最终验证结果

| 门禁 | 结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked --no-fail-fast` | exit 0；376 passed、0 failed、2 ignored | [workspace-test-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/workspace-test-final.log) |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | exit 0；45 passed、0 failed | [tauri-test-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/tauri-test-final.log) |
| `node src/testing/storeProbes.mjs` | exit 0；12/12 | [store-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/store-test.log) |
| `npm run build` | exit 0；TypeScript/Vite 生产构建通过 | [frontend-build.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/frontend-build.log) |
| `cargo fmt --all -- --check` | exit 0 | [fmt-workspace-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/fmt-workspace-final.log) |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | exit 0 | [fmt-tauri-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/fmt-tauri-final.log) |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | exit 0 | [clippy-workspace-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/clippy-workspace-final.log) |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features --locked -- -D warnings` | exit 0 | [clippy-tauri-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/clippy-tauri-final.log) |
| `cargo build --workspace --release --locked` | exit 0 | [build-workspace-release-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/build-workspace-release-final.log) |
| `cargo build --manifest-path src-tauri/Cargo.toml --release --locked` | exit 0 | [build-tauri-release-final.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/build-tauri-release-final.log) |
| 删除协议残留检索 | `NO_MATCHES` | [removed-protocol-rg.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/removed-protocol-rg.log) |

两项 ignored 是显式 benchmark/helper 类测试，不是失败。

## 5. 首次 workspace 运行失败说明

首次执行 workspace 测试时有 3 个 target 失败，日志保留在 [workspace-test.log](D:/Code/agent/opencode/dev-cleaner/target/review-20260908-round6/workspace-test.log)，未隐藏：

1. `fs_readonly`、`fs_toctou` 的 fixture 默认创建在 `C:\Users\curd9\AppData\Local\Temp`。当前受限执行环境不允许 validator 探测祖先 `C:\Users\curd9`，因此测试提前得到 `Unverifiable/PermissionDenied`，而不是各用例期望的业务结果。
2. `verified_delete` 中一条旧测试仍要求 untouched verified RecycleBin 成功，与新的 fail-closed 合同冲突。

处理方式：

- 将 `TMP`/`TEMP` 指向项目内部隔离目录 `D:\Code\agent\opencode\dev-cleaner\target\review-20260908-round6\tmp` 后重新运行；`fs_readonly`、`fs_toctou` 全部通过。
- 更新旧 verified RecycleBin 测试，改为验证结构化拒绝及原对象身份/内容保持不变。

最终 workspace 全量重跑为 exit 0。测试仅操作本轮隔离 fixture；普通 RecycleBin 测试回收的也是专门创建的 fixture，没有清理真实开发缓存。
