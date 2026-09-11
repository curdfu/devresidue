# DevResidue 规格说明书（SPEC）

**版本：** 0.3  
**状态：** Draft  
**目标平台：** Windows 10 / Windows 11 x64  
**核心技术栈：** Rust  
**项目构建产物识别：** `tbillington/kondo` / `kondo-lib`  
**开发工具缓存知识来源：** `tw93/Mole`（Windows 分支实现参考 + main 分支安全设计参考）  
**UI：** Tauri v2 + React + TypeScript  
**CLI：** Rust 原生 CLI  
**产品定位：** Windows Developer / AI Agent Storage Manager

---

# 1. 产品目标

DevResidue 是面向 Windows 开发者和 AI Coding Agent 重度用户的本地开发数据管理工具。

它解决的问题包括：

- `%USERPROFILE%` 下不断增加的 `.xxx` Agent/开发工具目录；
- Codex、Claude Code、OpenCode、Cursor、Windsurf 等产生的缓存、日志、会话和 workspace state；
- npm、pnpm、yarn、bun、pip、uv、Cargo、NuGet、Gradle、Maven 等产生的大量缓存；
- CMake、Rust、Node、Python、.NET、Java 等项目产生的大量可重建目录；
- Agent 自动创建临时 clone、worktree、build 目录后遗留；
- 用户无法判断某个开发目录到底能否安全删除。

产品核心不是：

> 删除尽可能多的数据

而是：

> **准确识别、分类、解释并安全管理开发环境数据。**

---

# 2. 产品边界

## 2.1 本项目负责

- AI Agent 数据；
- IDE / Coding Tool 数据；
- Developer Tool Cache；
- Package Manager Cache；
- Project Build Artifacts；
- Project Dependencies；
- Agent Session / Workspace State；
- 未知 `.xxx` 开发目录；
- 开发工具卸载残留；
- 可选 AI 辅助分类；
- 统一扫描、分类、预览、清理、日志和规则管理。

## 2.2 本项目不负责

MVP 不做：

- Windows Update 清理；
- Prefetch；
- Registry Cleaner；
- 系统服务优化；
- 浏览器清理；
- Event Log 清理；
- 驱动清理；
- 内存优化；
- 磁盘优化；
- 杀毒；
- 系统级实时监控；
- Secure Erase；
- 通用应用卸载；
- “一键优化 Windows”。

结论：

> **DevResidue 是 Developer Storage Manager，不是系统 Cleaner。**

---

# 3. 上游项目策略

## 3.1 Kondo

运行时依赖：

```text
kondo-lib
```

职责：

- 项目发现；
- 项目类型识别；
- build artifact 识别；
- 项目/产物大小；
- 项目更新时间；
- 多语言/多构建系统支持。

Kondo 只负责发现，不允许直接删除。

固定数据流：

```text
Kondo
  ↓
KondoProvider
  ↓
ScanItem
  ↓
RiskClassifier
  ↓
CleanupPlanner
  ↓
SafetyValidator
  ↓
CleanupEngine
```

## 3.2 Mole

不作为运行时依赖。

角色：

> **Developer Tool Cache Knowledge Base + Cleanup Safety Design Reference**

重点参考 Windows 分支：

- 开发缓存路径；
- npm/pnpm/yarn/bun；
- pip/uv；
- cargo/rustup；
- Go；
- Gradle；
- Maven；
- NuGet；
- whitelist；
- dry-run；
- process guard；
- tool-native cleanup command。

重点研究 main 分支：

- tool-reported cache path；
- physical path 校验；
- home/root 防护；
- symlink / reparse 风险；
- path identity；
- 删除前重新验证；
- TOCTOU 防护；
- fail-closed 策略。

License 原则：

- Windows MIT 分支可依法参考/迁移；
- GPL main 只研究设计思想，不直接复制实现。

## 3.3 DevCleaner

不再作为代码基础，也不作为运行时依赖。

仅可作为：

- AI Agent 数据路径参考；
- IDE 数据布局参考；
- session/conversation UX 参考。

---

# 4. 总体技术架构

```text
                       DevResidue
                           │
              ┌────────────┴────────────┐
              │                         │
          Rust Core                 Presentation
              │                         │
              │                 ┌───────┴───────┐
              │                 │               │
              │               CLI        Tauri + React
              │
              ├── Domain
              ├── Rule Engine
              ├── Risk Classifier
              ├── Cleanup Planner
              ├── Safety Validator
              ├── Cleanup Engine
              ├── Cleanup Journal
              │
              ├── KondoProvider
              │      └── kondo-lib
              │
              ├── DevCacheProvider
              │      └── Mole-derived knowledge
              │
              ├── AgentProvider
              ├── PackageProvider
              └── UnknownProvider
```

核心要求：

> Core 不依赖 Tauri、React 或 WebView。

---

# 5. Rust Workspace 结构

```text
DevResidue/
│
├── crates/
│   ├── devresidue-core/
│   │   ├── domain/
│   │   ├── rules/
│   │   ├── risk/
│   │   ├── cleanup/
│   │   ├── safety/
│   │   └── journal/
│   │
│   ├── devresidue-providers/
│   │   ├── agents/
│   │   ├── dev_cache/
│   │   ├── package/
│   │   ├── project/
│   │   └── unknown/
│   │
│   ├── devresidue-platform-windows/
│   │   ├── path/
│   │   ├── process/
│   │   ├── filesystem/
│   │   ├── shell/
│   │   └── recycle_bin/
│   │
│   └── devresidue-cli/
│
├── src-tauri/
│   └── src/
│       ├── commands/
│       ├── app_state.rs
│       └── main.rs
│
├── src/
│   ├── pages/
│   ├── components/
│   ├── stores/
│   ├── hooks/
│   └── types/
│
├── resources/
│   └── rules/
│       ├── agents/
│       ├── ide/
│       ├── dev_cache/
│       ├── packages/
│       └── protected/
│
└── tests/
    ├── rules/
    ├── providers/
    ├── filesystem/
    ├── cleanup/
    └── safety/
```

---

# 6. 核心领域模型

```rust
pub enum RiskLevel {
    Safe,
    RegenerableLocal,
    RegenerableDownload,
    Review,
    Protected,
    Unknown,
}
```

```rust
pub enum ResidueCategory {
    AiAgent,
    Ide,
    DeveloperCache,
    PackageCache,
    BuildArtifact,
    Dependency,
    Log,
    Temporary,
    Session,
    WorkspaceState,
    Configuration,
    Credential,
    Unknown,
}
```

```rust
pub enum SourceKind {
    Rule,
    Kondo,
    DeveloperCacheProvider,
    PackageManager,
    AgentProvider,
    UnknownProvider,
}
```

```rust
pub struct ScanItem {
    pub id: String,
    pub path: PathBuf,
    pub display_name: String,
    pub product: Option<String>,
    pub category: ResidueCategory,
    pub risk: RiskLevel,
    pub source: SourceKind,

    pub logical_size: u64,
    pub file_count: u64,
    pub last_modified: Option<SystemTime>,

    pub explanation: String,
    pub cleanup_action: CleanupAction,
    pub evidence: Vec<Evidence>,
}
```

---

# 7. Provider 架构

统一接口：

```rust
pub trait ResidueProvider {
    fn id(&self) -> &'static str;

    async fn scan(
        &self,
        ctx: &ScanContext,
    ) -> Result<Vec<ScanItem>>;
}
```

MVP Provider：

```text
AgentProvider
DevCacheProvider
PackageCacheProvider
KondoProvider
RuleProvider
UnknownDeveloperDataProvider
```

重要约束：

> Provider 只能发现、解释和估算数据，不能执行删除。

---

# 8. 风险等级

## SAFE

删除后基本无持久影响。

典型：

```text
logs
tmp
temp
crash dump
纯缓存
```

## REGENERABLE_LOCAL

删除后只需本地重新生成。

典型：

```text
CMake build
Rust target
.NET bin
.NET obj
CMakeFiles
```

## REGENERABLE_DOWNLOAD

删除后需要重新下载或重新安装依赖。

典型：

```text
node_modules
.venv
npm cache
pnpm store
Cargo registry
NuGet cache
Gradle cache
Maven cache
```

## REVIEW

可能包含用户有价值的数据。

典型：

```text
Agent sessions
conversation history
workspace state
temporary worktree
downloaded model
IDE workspace storage
```

## PROTECTED

禁止普通清理。

典型：

```text
config
settings
auth
credentials
SSH keys
API keys
tokens
MCP configuration
Git credentials
Cloud credentials
User scripts
Prompt configuration
Agent project instructions
```

## UNKNOWN

无法可靠识别。

默认：

```text
不删除
```

---

# 9. AI Agent 支持

MVP：

```text
Codex
Claude Code
OpenCode
OMO Slim
Cursor
Windsurf
```

后续：

```text
Cline
Roo Code
Kilo Code
Gemini CLI
Aider
Continue
GitHub Copilot CLI
Grok CLI
```

Agent 内部数据应尽量拆分：

```text
Cache             SAFE
Logs              SAFE
Temp              SAFE
Downloads         SAFE / REGENERABLE_DOWNLOAD
Sessions          REVIEW
History           REVIEW
Workspace State   REVIEW
Plugins           REVIEW
Config            PROTECTED
Auth              PROTECTED
```

禁止把整个 `.codex`、`.claude`、`.opencode` 等根目录直接标为 SAFE。

---

# 10. Developer Cache Provider

目录：

```text
providers/dev_cache/
├── npm.rs
├── pnpm.rs
├── yarn.rs
├── bun.rs
├── pip.rs
├── uv.rs
├── cargo.rs
├── rustup.rs
├── go.rs
├── gradle.rs
├── maven.rs
├── nuget.rs
└── mod.rs
```

设计来源：

- 路径和工具行为参考 Mole；
- 安全边界由 DevResidue 自己实现。

优先策略：

```text
Tool-native cleanup command
      ↓
Validated Direct Cleanup
```

不允许：

```text
发现 ~/.xxx
↓
直接递归删除
```

---

# 11. Tool-reported Cache Path

对于支持返回 cache root 的工具，优先：

```text
Tool
 ↓
Query Cache Root
 ↓
Normalize
 ↓
Safety Validate
 ↓
Scan
```

Tool 返回的路径仍然不可信。

必须拒绝：

```text
HOME
Drive Root
Windows
Program Files
ProgramData Root
Workspace Root
```

或其他明显过宽路径。

---

# 12. Process Guard

参考 Mole 的保守策略。

典型进程：

```text
codex
claude
opencode
cursor
windsurf
node
go
gopls
cargo
```

策略：

```text
Running
    → Skip / Defer

Unknown
    → 高风险对象 fail closed
```

禁止：

```text
Unknown == Not Running
```

---

# 13. KondoProvider

目录：

```text
providers/project/kondo.rs
```

输入：

```text
WorkspaceRoots
```

例如：

```json
{
  "workspaceRoots": [
    "D:\\Code",
    "D:\\Projects",
    "E:\\GitHub"
  ]
}
```

分类建议：

```text
build / target / bin / obj / CMakeFiles
    → REGENERABLE_LOCAL
```

```text
node_modules / .venv
    → REGENERABLE_DOWNLOAD
```

Kondo 的 delete 能力不允许暴露给 UI 或 CLI。

---

# 14. Rule Engine

目录：

```text
resources/rules/
├── agents/
├── ide/
├── dev_cache/
├── packages/
└── protected/
```

第一版支持：

```text
Exact Path
Environment Variable Expansion
Glob
Include
Exclude
Parent Marker
Exists
```

MVP 不启用 Regex。

规则优先级：

```text
Built-in Protected Rules
        >
User Protected Rules
        >
User Rules
        >
Community Rules
        >
Built-in Detection Rules
        >
AI Suggestion
```

远程 AI Suggestion 不是可执行规则，也不会在扫描、计划或清理阶段改变任何删除资格。只有用户逐项复核后，才可由本地闭合风险枚举、规则校验器与原子规则事务把选中的建议写成普通 `User Rules`；因此最终仍受上述 `User Rules` 优先级、保护规则和安全校验约束。AI 服务本身没有规则写入权。

---

# 15. Safety Validator

目录：

```text
safety/
├── path.rs
├── canonical.rs
├── reparse.rs
├── identity.rs
├── process.rs
├── protected.rs
└── validator.rs
```

删除前至少检查：

- normalized path；
- lexical path；
- physical path；
- path boundary；
- volume；
- root；
- protected path；
- reparse point；
- symlink；
- junction；
- mount point；
- target identity；
- target 是否发生变化；
- target 是否仍匹配规则；
- provider/risk 是否一致；
- 相关进程是否运行。

---

# 16. TOCTOU 模型

必须遵循：

```text
Scan
 ↓
Snapshot
 ↓
User Selection
 ↓
CleanupPlan
 ↓
Revalidate
 ↓
Cleanup
```

Snapshot 至少记录：

```text
normalized_path
attributes
reparse_state
last_write_time
file_identity（可获取时）
rule_id
provider_id
risk
```

删除前必须重新验证。

---

# 17. Reparse Point 策略

默认：

```text
DO NOT FOLLOW
```

包括：

- symlink；
- junction；
- mount point；
- 其他 reparse point。

---

# 18. Root Protection

普通规则永远不能整体清理：

```text
C:\
%USERPROFILE%
%SYSTEMROOT%
%PROGRAMFILES%
%PROGRAMFILES(X86)%
%PROGRAMDATA%
```

内置永久保护：

```text
%USERPROFILE%\.ssh
%USERPROFILE%\.gnupg
%USERPROFILE%\.aws
%USERPROFILE%\.azure
%USERPROFILE%\.kube
```

---

# 19. Cleanup Engine

唯一删除 Authority：

```text
CleanupEngine
```

支持：

```rust
pub enum CleanupMode {
    RecycleBin,
    DirectDelete,
    ExternalCommand,
}
```

策略：

```text
SAFE
    → 允许

REGENERABLE_LOCAL
    → 允许

REGENERABLE_DOWNLOAD
    → 用户确认

REVIEW
    → 显式确认

PROTECTED
    → Deny

UNKNOWN
    → Deny
```

---

# 20. Tauri 安全边界

禁止设计：

```rust
#[tauri::command]
fn delete_path(path: String)
```

前端不允许提交任意路径。

推荐：

```rust
#[tauri::command]
async fn create_cleanup_plan(
    item_ids: Vec<ScanItemId>
) -> Result<CleanupPlanDto>;
```

以及：

```rust
#[tauri::command]
async fn execute_cleanup_plan(
    plan_id: CleanupPlanId
) -> Result<CleanupSessionDto>;
```

UI 只能操作后端已经登记的 `ScanItemId` / `CleanupPlanId`。

---

# 21. CLI

CLI 是早期开发和测试的一等公民。

示例：

```powershell
devresidue scan
devresidue scan --projects D:\Code
devresidue scan --agents
devresidue scan --dev-cache

devresidue clean --dry-run --safe
devresidue clean --plan <id>

devresidue rules list
devresidue rules validate
```

CLI 必须复用与 GUI 完全相同的 Core。

---

# 22. ExternalCommand

只允许内置受信 Provider 创建。

禁止规则直接提供任意 shell 字符串。

推荐结构化：

```rust
ExternalCommand {
    executable,
    args,
    working_directory,
    timeout,
}
```

要求：

- 不经过 `cmd.exe /c` 拼接；
- 参数单独传递；
- timeout；
- stdout/stderr 捕获；
- exit code 校验。

---

# 23. Dry Run

所有 CleanupPlan 必须支持 Dry Run。

输出：

```text
Would Delete
Would Recycle
Would Execute
Would Skip
Reason
Estimated Space
```

---

# 24. Cleanup Journal

记录：

```text
session_id
time
product
rule
provider
path
action
estimated_size
result
error
```

禁止记录：

```text
token
credential
secret
private key
文件正文
```

---

# 25. Unknown Developer Data

重点扫描：

```text
%USERPROFILE%\.*
%LOCALAPPDATA%
%APPDATA%
```

但：

- 控制深度；
- 不做全盘暴力递归；
- 只输出疑似开发工具数据；
- 默认 `UNKNOWN`。

可操作：

```text
Open Folder
Ignore
Protect
联网 AI 研判（显式提交）
```

---

# 26. 可选远程 AI Advisor

远程 Advisor 默认全局禁用，按 Profile 配置 endpoint、model、结构化输出模式与超时。只有用户确认提交选中的 `UNKNOWN` / `REVIEW` 候选批次后，才会发送经白名单和脱敏处理的元数据；默认不含原始路径、证据、文件内容、源码、credential、token、私有文档、SSH key 与认证文件内容。用户可在每个批次单独勾选“发送完整本地路径”：此时路径仅能由已验证的当前扫描快照按 `ScanItemId` 在本地派生，预览必须展示，且路径不写入 Profile、审计、AI DTO 或远程响应。UI/CLI 绝不接收任意 Path 参数。每个条目以批次内随机 token 标识，远程响应也只能引用该 token。

Profile 元数据不包含 API Key。API Key 使用由 Profile UUID 派生的用户级环境变量 `DEVRESIDUE_AI_KEY_<UPPERCASE_UUID>` 保存，并只在当前进程内读取和使用；这不是凭据隔离边界，同一 Windows 用户下可读取用户环境变量的进程同样可能读取该值。Profile 的创建、更新、删除与恢复使用本地事务；删除 Profile 会移除其派生 Key，但不会删除已确认的用户规则。

远程响应先在本地验证 token、风险枚举、置信度和结构，随后仅作为进程内建议展示。用户必须逐项给出最终风险；`UNKNOWN` 不能作为确认结果。只有本地验证和原子规则事务都成功时，才会创建 `User Rules`，冲突或失败时整批不写入。AI 本身没有删除、创建 CleanupPlan、执行 CleanupPlan、覆盖 `PROTECTED` 或直接写规则的权威；任何后续清理仍必须由 CleanupPlanner、确认流程和 CleanupEngine 按既有安全不变量处理。

---

# 27. UI

产品 UI 定位：

> **Developer Storage Manager**

建议页面：

```text
Dashboard
AI Agents
Developer Cache
Projects
Packages
Unknown
Rules
Settings
```

Dashboard 示例：

```text
Developer Storage: 64.3 GB

AI Agents             7.8 GB
Developer Cache      14.2 GB
Project Artifacts    37.5 GB
Unknown Dev Data      4.8 GB

Safe Now              8.2 GB
Local Rebuild         17.4 GB
Needs Redownload      19.6 GB
Review                 8.1 GB
Protected              11 MB
```

---

# 28. 性能目标

目标规模：

```text
500k - 1M files
```

要求：

- known-location scan 快速；
- workspace scan progressive；
- UI 不阻塞；
- 支持 Cancel；
- bounded concurrency；
- 禁止 one-task-per-file。

推荐：

```text
Tokio
Semaphore
CancellationToken
Tauri event stream
```

---

# 29. MVP 范围

## Agents

```text
Codex
Claude Code
OpenCode
OMO Slim
Cursor
Windsurf
```

## Developer Cache

```text
npm
pnpm
yarn
bun
pip
uv
cargo
rustup
Go
Gradle
Maven
NuGet
```

## Project Artifacts

通过 Kondo 支持：

```text
CMake
Node
Rust
Python
.NET
Gradle
Maven
Unreal
以及其他支持类型
```

---

# 30. MVP 必须具备

```text
Scan
Size
File Count
Age
Product
Category
Risk
Explanation
Preview
Dry Run
Clean
Recycle Bin
External Command
Cleanup Journal
Workspace Root
Process Guard
Safety Revalidation
CLI
Tauri UI
```

---

# 31. Safety Invariants

## INV-001
UNKNOWN 永不自动删除。

## INV-002
PROTECTED 永不进入普通 Cleanup Queue。

## INV-003
AI 不得直接触发删除。

## INV-004
Reparse Point 默认不得跟随。

## INV-005
删除前必须重新验证目标。

## INV-006
Rule 不得匹配整个 HOME、Windows、Program Files、ProgramData 或磁盘根。

## INV-007
Credential / Authentication 永久 fail-safe。

## INV-008
Kondo 只能发现，不得直接删除。

## INV-009
Provider 不得直接删除。

## INV-010
只有 CleanupEngine 可以执行删除。

## INV-011
Tool-reported cache path 也必须经过 SafetyValidator。

## INV-012
进程状态未知时，高风险目标默认跳过。

## INV-013
UI/CLI 不允许通过任意 path 参数绕过 ScanItem / CleanupPlan。

---

# 32. 发布阻断测试

以下任一失败禁止发布：

```text
Root Protection Test
User Profile Protection Test
ProgramData Root Protection Test
Reparse Traversal Test
Junction Substitution Test
Symlink Substitution Test
TOCTOU Replacement Test
Protected Rule Test
Cleanup DryRun Consistency Test
Arbitrary Path Command Rejection Test
```

---

# 33. 测试要求

## Unit

- Rule parsing；
- Rule priority；
- Risk classification；
- path matching；
- env expansion；
- protected paths；
- cleanup planning。

## Provider

- fixture → ScanItem；
- risk；
- explanation；
- cleanup strategy。

## Filesystem

- normal；
- locked；
- read-only；
- permission denied；
- long path；
- UNC；
- symlink；
- junction；
- mount point；
- reparse；
- hardlink；
- concurrent rename；
- replacement。

## Integration

完整链路：

```text
Provider
↓
ScanCoordinator
↓
Rule Engine
↓
Risk Classifier
↓
CleanupPlanner
↓
SafetyValidator
↓
DryRun
```

---

# 34. 发布目标

首发：

```text
Windows x64
```

后续：

```text
Windows ARM64
```

发布形式：

```text
MSI / NSIS
Portable ZIP
CLI ZIP
```

发布要求：

```text
Code Signing
SBOM
Cargo audit
npm audit
Dependency lock
Release hash
```

MVP 默认：

```text
No Telemetry
AI Disabled
```

---

# 35. 成功标准

用户应能快速知道：

```text
AI Agent 占多少
Developer Cache 占多少
Project Artifact 占多少
哪些可以立即删除
哪些只需本地重建
哪些需要重新下载
哪些需要人工判断
哪些绝不能碰
```

最终产品差异化：

> **分类准确、解释清晰、安全边界强、扩展成本低。**
