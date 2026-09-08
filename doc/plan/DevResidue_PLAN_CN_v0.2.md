# DevResidue 实施计划（PLAN）

**版本：** 0.2  
**对应规格：** `DevResidue_SPEC_CN_v0.3.md`  
**状态：** Draft  
**技术路线：** Rust Core + CLI First + kondo-lib + Mole-derived Developer Cache + Tauri v2 / React UI  
**目标平台：** Windows 10 / Windows 11 x64

---

# 1. 实施总原则

本版本不再基于 DevCleaner Fork。

实施顺序调整为：

```text
Rust Workspace
    ↓
Core Domain
    ↓
CLI Prototype
    ↓
Rule Engine
    ↓
Safety Layer
    ↓
Cleanup Engine
    ↓
Kondo
    ↓
Developer Cache
    ↓
AI Agent Providers
    ↓
Tauri UI
    ↓
Unknown / AI
```

核心原则：

1. Core 先于 UI。
2. CLI 先于 GUI。
3. Safety 先于大规模 Provider。
4. Provider 只能发现，不允许删除。
5. UI/CLI 不能提交任意路径执行删除。
6. 只有 CleanupEngine 拥有删除权限。
7. UNKNOWN / PROTECTED 永不自动删除。
8. 每个阶段必须具备独立验收标准。

---

# 2. 里程碑

## M0：工程骨架

完成：

```text
Rust Workspace
devresidue-core
devresidue-providers
devresidue-platform-windows
devresidue-cli
```

可以构建、测试、运行。

## M1：安全 Core

完成：

```text
Domain
Rule Engine
Risk Classifier
SafetyValidator
CleanupPlanner
CleanupEngine
Journal
```

此时即使只有 fixture，也能完整验证安全链路。

## M2：可用 CLI

接入：

```text
Kondo
Developer Cache
AI Agents
```

CLI 已能完成扫描、分类、dry-run 和安全清理。

## M3：桌面产品

接入：

```text
Tauri v2
React UI
Dashboard
Storage Views
Settings
```

## M4：增强能力

加入：

```text
Unknown Developer Data
AI Analyzer
Community Rules
```

---

# 3. Phase 0：Rust Workspace 初始化

## 目标

建立完全自主的工程，不继承第三方应用架构。

建议：

```bash
cargo new --lib crates/devresidue-core
cargo new --lib crates/devresidue-providers
cargo new --lib crates/devresidue-platform-windows
cargo new --bin crates/devresidue-cli
```

根目录：

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
rustfmt.toml
clippy.toml
```

建立：

```text
tests/
resources/rules/
docs/
```

CI 初期至少运行：

```text
cargo fmt --check
cargo clippy
cargo test
cargo build --release
```

## 验收

- Workspace 全部 crate 可构建；
- Release 构建成功；
- CI 成功；
- 无 Tauri 依赖进入 Core。

---

# 4. Phase 1：核心 Domain Model

实现：

```text
RiskLevel
ResidueCategory
SourceKind
ScanItem
Evidence
CleanupAction
CleanupMode
CleanupPlan
CleanupSession
CleanupResult
```

定义稳定 ID：

```text
ScanItemId
CleanupPlanId
RuleId
ProviderId
```

要求：

- ID 不等同于 Path；
- UI/CLI 后续只能基于 ID 请求 cleanup；
- Core 不依赖平台 UI。

## 验收

- Domain 单元测试通过；
- serde DTO 可用；
- ID 与 Path 完全分离。

---

# 5. Phase 2：CLI Skeleton

## 目标

尽早建立可观察的执行入口。

初始命令：

```powershell
devresidue scan
devresidue scan --json
devresidue rules list
devresidue rules validate
```

此时 Provider 可只返回 fixtures。

输出必须显示：

```text
Product
Category
Path
Size
Risk
Source
Reason
```

## 验收

- CLI 能调用 Core；
- JSON 输出稳定；
- 无清理功能前不实现任意 delete 命令。

---

# 6. Phase 3：Rule Engine

实现：

```text
schema.rs
loader.rs
validator.rs
matcher.rs
priority.rs
```

第一版支持：

```text
Exact Path
Env Expansion
Glob
Include
Exclude
Parent Marker
Exists
```

不做 Regex。

规则来源：

```text
builtin
community
user
ai-suggestion
```

优先级严格按 SPEC。

建立 Built-in Protected Rules。

## 验收

必须通过：

```text
危险根规则拒绝
Protected 不可覆盖
Include/Exclude 正确
环境变量路径正确
```

---

# 7. Phase 4：Windows Platform Layer

建立：

```text
devresidue-platform-windows/
├── path/
├── filesystem/
├── process/
├── identity/
├── recycle_bin/
└── shell/
```

实现：

- Windows path normalization；
- case-insensitive containment；
- long path；
- UNC；
- file attributes；
- reparse detection；
- process detection；
- file identity；
- Recycle Bin；
- 外部进程执行。

此层只提供能力，不做业务分类。

## 验收

所有能力可单测或 fixture 测试。

---

# 8. Phase 5：SafetyValidator

最高优先级阶段。

实现：

```text
ProtectedRootRegistry
PathValidator
ReparseGuard
IdentitySnapshot
ProcessGuard
SafetyVerdict
SafetyValidator
```

Snapshot：

```text
normalized_path
attributes
reparse_state
last_write_time
file_identity
rule_id
provider_id
risk
```

清理前重新验证。

必须覆盖：

```text
scan 后 rename
scan 后 replace
scan 后 symlink swap
scan 后 junction swap
scan 后删除并重建
```

## 发布阻断测试

- `C:\` 不可删；
- `%USERPROFILE%` 不可删；
- `%PROGRAMDATA%` 不可删；
- symlink/junction 不越界；
- sibling prefix 不误判；
- stale snapshot 不执行。

---

# 9. Phase 6：CleanupPlanner + CleanupEngine

实现唯一删除路径：

```text
Selected ScanItemId[]
        ↓
CleanupPlanner
        ↓
CleanupPlan
        ↓
SafetyValidator
        ↓
CleanupEngine
```

禁止 API：

```text
delete_path(path)
remove_directory(path)
```

CLI 只允许：

```powershell
devresidue clean --plan <id>
```

或：

```powershell
devresidue clean --safe
```

其内部仍先生成 Plan。

实现：

```text
RecycleBin
DirectDelete
ExternalCommand
DryRun
Partial Failure
Journal
```

ExternalCommand 不使用 shell 字符串拼接。

## 验收

- Provider 无删除函数；
- 任意 path 无法进入 CleanupEngine；
- Dry Run 与真实 Plan 一致；
- Partial Failure 可继续；
- Journal 完整。

---

# 10. Phase 7：KondoProvider

添加锁定版本：

```text
kondo-lib
```

封装：

```text
providers/project/kondo.rs
```

输入：

```text
WorkspaceRoots
```

输出统一 `ScanItem`。

Risk Mapping：

```text
build / target / bin / obj
    → REGENERABLE_LOCAL

node_modules / .venv
    → REGENERABLE_DOWNLOAD
```

禁止调用 Kondo delete。

测试 fixture：

```text
CMake
Node
Rust
Python
.NET
Gradle
Maven
```

## 验收

- 正确发现；
- 不重复统计；
- 支持取消；
- Kondo 只读接入。

---

# 11. Phase 8：Developer Cache Providers

按优先级实现：

第一批：

```text
npm
bun
pip
uv
cargo
NuGet
```

第二批：

```text
pnpm
yarn
rustup
Go
Gradle
Maven
```

每个 Provider 必须实现：

```text
detect
query path
validate path
estimate
classify
cleanup strategy
process guard（需要时）
```

优先 tool-native cleanup。

Mole：

- Windows 分支用于路径/命令参考；
- main 用于安全设计参考；
- 不形成运行时依赖。

## 验收

第一批全部具备：

```text
scan
risk
reason
dry-run
cleanup
```

---

# 12. Phase 9：AI Agent Providers

第一批：

```text
Codex
Claude Code
OpenCode
OMO Slim
Cursor
Windsurf
```

实现原则：

- 优先 Rule；
- 复杂发现逻辑再写 Provider；
- 整个根目录绝不直接标 SAFE；
- config/auth 永远 Protected；
- session/history 默认 Review。

目录：

```text
resources/rules/agents/
```

## 验收

每个 Agent 至少能拆出：

```text
Cache
Logs
Temp
Sessions
Config
Auth
```

并正确分类。

---

# 13. Phase 10：CLI MVP

扩展命令：

```powershell
devresidue scan
devresidue scan --agents
devresidue scan --dev-cache
devresidue scan --projects D:\Code
devresidue scan --json

devresidue clean --dry-run --safe
devresidue clean --plan <id>

devresidue rules list
devresidue rules validate
```

增加：

```text
progress
cancel
summary
journal
```

CLI 是正式产品能力，不是临时工具。

## 验收

无需 GUI 即可完成完整 MVP 流程。

---

# 14. Phase 11：Tauri v2 + React UI 初始化

Core 稳定后再开始 UI。

创建：

```text
src-tauri/
src/
```

Tauri commands 只允许 ID-based API。

允许：

```text
scan
get_scan_results
create_cleanup_plan
execute_cleanup_plan
cancel_scan
get_cleanup_session
```

禁止：

```text
delete_path
run_command
remove_directory
```

## 验收

前端无法构造任意路径删除请求。

---

# 15. Phase 12：Dashboard 与主页面

实现：

```text
Dashboard
AI Agents
Developer Cache
Projects
Packages
Rules
Settings
```

Dashboard 显示：

```text
Safe Now
Local Rebuild
Needs Redownload
Review
Protected
```

Result Row：

```text
Name
Product
Path
Size
Age
Risk
Reason
Provider
```

Detail Panel 必须说明：

```text
是什么
属于谁
为什么这么分类
删除影响
由谁检测
对应哪个 Rule
```

## 验收

用户可在一分钟内理解主要空间占用。

---

# 16. Phase 13：Unknown Developer Data

实现：

```text
UnknownDeveloperDataProvider
```

重点位置：

```text
%USERPROFILE%\.*
%LOCALAPPDATA%
%APPDATA%
```

要求：

- 限制深度；
- 不全盘扫描；
- 只输出疑似开发数据；
- 全部默认为 UNKNOWN。

操作：

```text
Open Folder
Ignore
Protect
Create Rule
```

AI 暂不启用。

---

# 17. Phase 14：AI Analyzer

Core 完成后再加入。

接口：

```rust
trait DirectoryAnalyzer
```

输入仅 metadata。

输出：

```text
product guess
confidence
classification suggestion
suggested rule
```

AI 无权：

```text
删除
执行 CleanupPlan
覆盖 Protected
直接启用危险规则
```

## 验收

AI 完全关闭时程序功能不受影响。

---

# 18. Phase 15：Hardening

专项覆盖：

```text
TOCTOU
junction swap
symlink swap
mount point
path canonicalization
UNC
long path
malicious rule
community rule override
external command injection
large tree DoS
permission storm
locked files
concurrent delete
```

对以下模块强制代码 Review：

```text
SafetyValidator
CleanupEngine
RuleValidator
ExternalCommand
Path containment
Reparse handling
```

---

# 19. Phase 16：性能优化

目标规模：

```text
500k - 1M files
```

实现：

```text
Tokio
bounded concurrency
Semaphore
CancellationToken
streaming result
```

优化顺序：

1. known locations；
2. workspace scan；
3. size calculation；
4. progressive result；
5. cancellation；
6. dedup；
7. optional cache。

禁止 one-task-per-file。

---

# 20. Phase 17：发布

首发：

```text
Windows x64
```

发布：

```text
CLI ZIP
Portable ZIP
NSIS / MSI
```

要求：

```text
Code Signing
SBOM
cargo audit
npm audit
Cargo.lock
frontend lockfile
release hash
```

MVP：

```text
No Telemetry
AI Disabled by default
```

---

# 21. 推荐迭代顺序

严格推荐：

```text
Phase 0
↓
Phase 1
↓
Phase 2
↓
Phase 3
↓
Phase 4
↓
Phase 5
↓
Phase 6
↓
Phase 7
↓
Phase 8
↓
Phase 9
↓
Phase 10
↓
Phase 11
↓
Phase 12
↓
Phase 13
↓
Phase 14
↓
Phase 15
↓
Phase 16
↓
Phase 17
```

重点：

> UI 不得提前到 Safety/Core 之前。

---

# 22. 首个可用版本建议

第一阶段实际可用版本只要求：

```text
Rust Core
CLI
Rule Engine
SafetyValidator
CleanupEngine
Kondo
Codex
Claude Code
OpenCode
npm
bun
pip
uv
cargo
Dry Run
Journal
```

暂缓：

```text
AI Analyzer
Community Rules
Unknown 自动发现
大量 IDE
完整 Package Manager 覆盖
复杂 Backup
漂亮 UI
```

---

# 23. Definition of Done

MVP 必须满足：

## 功能

```text
AI Agent Scan
Developer Cache Scan
Project Artifact Scan
Risk Classification
Preview
Dry Run
Cleanup
Journal
Workspace Roots
Process Guard
CLI
GUI
```

## 安全

```text
UNKNOWN 不自动删
PROTECTED 不自动删
Reparse 不跟随
Root 不可删
删除前 Revalidate
Provider 无删除权限
Kondo 无删除权限
AI 无删除权限
UI/CLI 无 arbitrary path delete
```

## 可解释性

每个结果必须回答：

```text
是什么？
谁产生的？
为什么这么分类？
删掉有什么影响？
由谁检测？
使用哪个 Rule？
```

---

# 24. Agent 开发分工建议

## 高强度模型

用于：

```text
架构
SafetyValidator
TOCTOU
Windows path semantics
CleanupEngine
ExternalCommand
Rule security
Security Review
```

## 中等模型

用于：

```text
Provider architecture
Rule Engine
Kondo integration
CLI
Tauri command layer
复杂重构
```

## 高速执行模型

用于：

```text
批量 Provider
规则文件
fixture
单元测试
UI 页面
文档
重复性代码
```

以下模块 merge 前必须强 Review：

```text
SafetyValidator
CleanupEngine
RuleValidator
Path containment
Reparse handling
ExternalCommand
Tauri cleanup commands
```

---

# 25. 最终项目交付物

仓库应至少包含：

```text
SPEC.md
PLAN.md
README.md
SECURITY.md
CONTRIBUTING.md
THIRD_PARTY_NOTICES.md
docs/
resources/rules/
```

---

# 26. 非协商约束

以下不得因实现方便被弱化：

```text
1. Provider 不允许删除。
2. Kondo 不允许直接删除。
3. AI 不允许直接删除。
4. UI/CLI 不允许直接按 Path 删除。
5. UNKNOWN 不允许自动删除。
6. PROTECTED 不允许普通删除。
7. Reparse Point 默认不跟随。
8. 删除前必须重新验证。
9. Root Protection 属于发布阻断项。
10. CleanupEngine 是唯一删除 Authority。
```

如果实现与这些约束冲突：

> **停止实现并报告架构冲突，不得自行绕过。**
