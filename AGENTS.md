# DevResidue Agent 开发规范

本项目是 DevResidue：Windows 开发者 / AI Agent 存储管理工具（Rust Core + CLI First + Tauri v2）。

**必读文档（实施前）：**

- `doc/spec/DevResidue_SPEC_CN_v0.3.md` — 规格说明书，尤其 §31 Safety Invariants（INV-001..INV-013）
- `doc/plan/DevResidue_PLAN_CN_v0.2.md` — 实施计划，Phase 0-6 当前生效范围

**硬约束（违反即停止实现并报告架构冲突）：**

1. Provider 不允许删除；Kondo 不允许直接删除；AI 不允许直接删除。
2. UI/CLI 不允许按任意 Path 删除，只能按 ScanItemId / CleanupPlanId 操作。
3. UNKNOWN / PROTECTED 永不自动删除。
4. Reparse Point 默认不跟随；删除前必须重新验证（TOCTOU）。
5. CleanupEngine 是唯一删除 Authority。
6. Core 不依赖 Tauri / React / WebView。

## Cloned Dependency Source

Read-only dependency source repositories are available under
`.slim/clonedeps/repos/` for inspection. Do not edit these clones.

- `.slim/clonedeps/repos/tbillington__kondo/` — `tbillington/kondo` at tag `v0.9` (MIT); kondo-lib source, the runtime dependency for project/build artifact discovery.
- `.slim/clonedeps/repos/tw93__Mole__windows/` — `tw93/Mole` at branch `windows` (MIT); developer cache paths and tool-native cleanup command reference.
- `.slim/clonedeps/repos/tw93__Mole__main/` — `tw93/Mole` at branch `main` (GPL); safety design reference only — ideas, not code. Never copy implementation from this clone.

GitHub 参考项目克隆一律放在 `.slim/clonedeps/repos/` 下，禁止与开发目录（`crates/`、`resources/` 等）混放。
