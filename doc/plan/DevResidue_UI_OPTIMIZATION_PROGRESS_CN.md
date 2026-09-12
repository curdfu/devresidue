# DevResidue UI / UX 优化实施进度

计划来源：`doc/plan/DevResidue_UI_OPTIMIZATION_PLAN_CN_v0.1.md`  
基线日期：2026-09-12  
基线工作区：`C:\Code\agent\opencode\devresidue`

## 基线

- 只读 Git 标识：`d01d9a68bee8a1963ebb75011530e8ec75920bc9`
- 基线状态：工作区只有用户提供的 UI 优化计划文件
  `doc/plan/DevResidue_UI_OPTIMIZATION_PLAN_CN_v0.1.md` 未跟踪；本记录不执行
  `git add`、`commit`、`checkout`、`reset` 或其他 Git 写操作。
- 运行环境：Node `v24.19.0`，npm `11.13.0`，Cargo `1.98.1`
  (`797e8a9bc 2026-08-05`)。
- 当前未把任何构建、探针、浏览器、Tauri 原生或真实后端验收标为通过；这些检查
  留待对应批次执行并记录实际结果。

## 当前 UI 页面与验证入口

当前 `src/components/AppShell.tsx` 通过页面状态切换以下入口：

1. `dashboard`：`DashboardPage`
2. `local`：`CategoryPage`
3. `rebuild`：`CategoryPage`
4. `download`：`CategoryPage`
5. `review`：`RiskResultsPage`
6. `unknown`：`UnknownPage`
7. `ai-review`：`AiReviewPage`（AI 辅助流程，当前计划不作为一级导航）
8. `rules`：`RulesPage`
9. `journal`：`JournalPage`
10. `settings`：`SettingsPage`

共享 UI 入口包括 `AppShell`、`Sidebar`、`ScanBar`、`ResultsTable`、
`DetailPanel`、`CleanupFlow`、`AiProfilePanel` 和 `AiReviewPanel`。

计划规定的窗口核对尺寸为 `960×640`、`1200×900`、`1440×900`；本基线尚未在
这些尺寸、主题、DPI 或 WebView2 环境中执行截图验收。

## 当前检查命令

`package.json` 当前仅提供：

- `npm run dev`：启动 Vite 开发服务；未在本基线运行。
- `npm run build`：`tsc --noEmit && vite build`；未在本基线运行。
- `npm run preview`：预览构建结果；未在本基线运行。

已有验证入口：

- `node src/testing/storeProbes.mjs`：现有 store 探针入口，B00.4 才执行基线检查。
- `src/testing/store-probes.bundle.mjs`：仓库中已有的生成 bundle，B00.2 负责将后续
  输出改到 `node_modules/.cache/devresidue-ui-probes` 的唯一运行目录；本批不手动
  编辑或删除它。

计划中可按批次选择的 Rust 检查包括对应 crate 的 `cargo test`、Tauri manifest
测试和格式检查；本基线尚未宣称这些命令通过。

## 批次状态

| 批次 | 状态 | 进度 |
| --- | --- | --- |
| B00 | 已完成 | B00.1—B00.4 已完成并通过构建/探针入口验证 |
| B01 | 已完成 | B01.1—B01.6 已实现；异步、忙碌态和清理语义有自动化覆盖 |
| B02 | 已完成 | B02.1—B02.5 已实现；浏览器键盘/焦点验收待真实环境 |
| B03 | 已完成 | B03.1—B03.6 已实现；跨进程/原生窗口矩阵待验收 |
| B04 | 已完成 | B04.1—B04.6 已实现；跨页选择与代次隔离已覆盖 |
| B05 | 已完成 | B05.1—B05.5 已实现；统计和分组统一使用 selector |
| B06 | 已完成 | B06.1—B06.6 已实现；原生目录选择保留无插件安全降级 |
| B07 | 已完成 | B07.1—B07.5 已实现；真实规则文件权限/重扫待隔离数据根验收 |
| B08 | 已完成 | B08.1—B08.6 已实现；真实联网服务矩阵未验证 |
| B09 | 已完成 | B09.1—B09.6 已实现；发布包数据目录待 Windows 原生验收 |
| B10 | 基础实现完成 | B10.1—B10.5 已实现；B10.6 大列表性能待 1,000/10,000 条测量 |
| B11 | 可执行回归完成 | workspace、CLI、Tauri、前端和 store probes 通过；WebView2/发布包/真实 AI 待环境验收 |

## F01—F12 覆盖状态

| 编号 | 实现状态 | 验证状态 | 对应批次 |
| --- | --- | --- | --- |
| F01 | 已实现 | 自动化/源码通过；混合计划真实 UI 与原生确认未验证 | B01 |
| F02 | 已实现 | 自动化/源码通过；真实清理与原生确认未验证 | B01、B07 |
| F03 | 已实现 | 列表/详情布局已构建；窗口矩阵与真实截图未验证 | B04、B10 |
| F04 | 已实现 | 选择并集/差集、已选页和代次隔离已覆盖 | B04 |
| F05 | 已实现 | 键盘/焦点和语义规则已实现；屏幕阅读器/强制颜色未验证 | B02、B10、B11 |
| F06 | 已实现 | 清日志/重置分离及状态失效已实现；原生互斥待验收 | B03、B09 |
| F07 | 已实现 | 统计、分类和项目分组使用统一 selector | B05 |
| F08 | 已实现 | 扫描、计划、预演忙碌态和异步反馈已有自动化覆盖；真实 UI 未验证 | B01、B06 |
| F09 | 已实现 | 会话记录、失败定位、规则校验和删除闭环已实现 | B07 |
| F10 | 已实现 | AI 三步流程、返回上下文和脱敏边界已实现；真实联网矩阵未验证 | B08 |
| F11 | 已实现 | 设置分组、目录校验、主题和应用数据展示已实现；原生目录选择待验收 | B06、B09 |
| F12 | 基础实现完成 | 视觉 token、响应式和主题已实现；对比度、DPI、大列表性能未验证 | B05、B10、B11 |

## 固定安全边界

后续 UI 优化必须继续遵守：

- UNKNOWN、PROTECTED 不自动删除；AI 不直接删除、创建或执行清理计划。
- UI/CLI 只能按 `ScanItemId` / `CleanupPlanId` 请求清理，不新增任意路径删除接口。
- CleanupEngine 是唯一删除 authority；删除前保留 SafetyValidator、TOCTOU、Reparse、
  generation 和进程状态校验。
- Core 不依赖 React、Tauri 或 WebView。
- API Key 不进入 Zustand、`localStorage`、日志、截图或测试快照。
- 真实后端、Mock、浏览器和 Tauri 原生验证必须分开记录。

## B00.1 / B00.2 交接

- B00.1 状态：已完成；B00.2 已接续完成并通过验证。
- B00.1 修改：新增本进度文档；未修改业务代码、测试代码或依赖。
- B00.1 验证：只读检查了 Git 基线、路由、页面文件、测试入口、Node/npm/Cargo 版本；
  当时未运行 `npm run build`、`storeProbes`、Rust 测试、浏览器或原生验收。
- B00.1 后续：B00.2、B00.3 已完成并通过验证。

### B00.2：修正探针输出位置

- 状态：已验收。
- 修改文件：`src/testing/storeProbes.mjs`。
- 行为变化：每次运行在 `node_modules/.cache/devresidue-ui-probes` 下创建唯一的
  `run-*` 目录，bundle 只写入本次目录；通过 `try/finally` 仅清理本次目录。
- 依赖解析：bundle 仍位于仓库 `node_modules` 层级内，保留 `packages: "external"`
  的项目依赖解析能力。
- 验证命令：连续两次执行 `node src/testing/storeProbes.mjs`，每次均退出成功并输出
  `12/12 batch-3 store-probe verifications passed (fixes, not defects).`。
- 副作用核对：两次运行后 `RUN_DIR_COUNT=0`；旧的
  `src/testing/store-probes.bundle.mjs` 保持 `68149` 字节和 `2026-09-10 11:56:33`
  时间戳，未被改写。
- 未验证内容：本批没有运行 `npm run build`、Rust 测试、浏览器或 Tauri 原生验收。
- B00.2 后续：B00.3 已完成并通过验证。

### B00.3：整理可复用 fixture / scenario

- 状态：已验收。
- 修改文件：新增 `src/testing/uiFixtures.mjs`；未修改正式 UI、store、MockBackend 或
  产品设置。
- 风险矩阵：提供 `safe`、`regenerable-local`、`regenerable-download`、`review`、
  `protected`、`unknown` 六类确定性 `ScanItemDto` fixture，路径统一位于
  `C:/devresidue-fixture` 下，不执行真实 I/O。
- 场景集合：`risk-matrix`、`mixed-plan`、`empty-list`、`long-path`、
  `permission-failure`、`delayed-completion`、`partial-results`、`new-generation`。
- 安全边界：混合计划只把前四类放入 `items`，`protected` 与 `unknown` 始终进入
  `skipped`；新 generation 场景明确展示数字 ID 复用，供代次失效探针使用。
- 后端替身：`createInertBackend` 只记录调用并返回内存 fixture，可注入结构化权限错误
  或延迟 Promise；不访问文件、命令、网络或正式设置。
- 验证命令：`node --check src/testing/uiFixtures.mjs`；Node 纯内存断言覆盖 8 个场景、
  风险矩阵、计划字段集合、保护/未知边界、长路径、结构化错误和延迟调用，输出
  `PASS B00.3 fixture scenarios: 8 scenarios, risk matrix, safety boundaries, delay/error substitutes`。
- 未验证内容：本批没有运行 `npm run build`、Rust 测试、浏览器或 Tauri 原生验收。
- B00.3 后续：B00.4 已完成并通过验证。

### B00.4：运行并记录基线检查

- 状态：已验收。
- `npm run build`：退出成功；`tsc --noEmit` 与 Vite production build 均通过，Vite
  报告 `1718 modules transformed`，构建耗时 `1.73s`。
- `node src/testing/storeProbes.mjs`：退出成功，AI store / MockBackend 安全生命周期
  检查与 12 个扫描代次、事件顺序、错误和旧事件隔离场景全部通过。
- UI-000：构建与原有探针结果明确，基线命令均通过。
- UI-001：B00.2 已连续运行探针两次；本批再次运行后 `RUN_DIR_COUNT=0`，旧
  `src/testing/store-probes.bundle.mjs` 仍为 `68149` 字节、`2026-09-10 11:56:33`，
  没有共享输出冲突。
- UI-002：本进度文档已记录 B00—B11、F01—F12、建议编号和验证状态；原 UI 评审仍
  标记为演示环境证据。
- 意外改动核对：`git status --short` 只包含本计划的脚本、fixture、计划文档和进度
  文档；`dist` 构建产物未进入 Git 状态。
- 未验证内容：浏览器真实 DOM / 焦点、Tauri WebView2 原生窗口、Windows DPI、真实
  后端清理和真实联网 AI 均未验证，不能标为通过。
- 下一步：B01.1，先建立统一清理展示函数。

### B01.1：建立统一清理展示函数

- 状态：已验收（子任务）；B01 批次仍进行中。
- 修改文件：新增 `src/utils/cleanupPresentation.ts`；修改
  `src/components/CleanupFlow.tsx`、`src/pages/JournalPage.tsx`。
- 统一输出：展示函数保留原始 wire 值，并返回 `label`、`summary`、`raw`；动作、确认级别、跳过/拒绝/暂缓/预演原因均不再由组件各自映射。
- 动作语义：`recycle` 与 `delete` 均显示“永久删除”；已核对当前
  `CleanupEngine` 对 `RecycleBin` 实际调用 `WindowsDeletePort.delete_tree_verified`，因此没有“可从回收站恢复”的承诺。`execute` 显示“执行工具清理”；未知动作显示“未识别操作（原值：…）”。
- 复用范围：清理计划、预演/最终结果、清理失败确认和日志页均使用同一展示函数；原始值通过 tooltip 或未知值的显式文本保留。
- 安全边界：未修改 `PlanAction` / `Confirmation` wire 枚举、计划 DTO、CleanupEngine、删除端口或执行流程；展示层不会把未知值猜测成可执行动作。
- 验证命令：`npm run build`；`tsc --noEmit` 与 Vite production build 均通过，报告 `1719 modules transformed`。
- 未验证内容：浏览器真实 DOM、tooltip 可见性、Tauri WebView2、Windows 原生清理与真实日志数据尚未验收；构建通过不等同于真实删除行为通过。
- 下一步：B01.2，按计划分离风险影响与授权策略。

### B01.2：分离实际风险与授权策略

- 状态：已验收（子任务）；B01 批次仍进行中。
- 修改文件：`src/stores/cleanupStore.ts`、`src/components/CleanupFlow.tsx`、
  `src/styles/global.css`。
- 计划上下文：建计划时保存当前 `scanGeneration` 与所选条目的风险快照；预演/执行前校验代次、条目匹配、风险分类和动作/确认枚举。缺失上下文、代次变化、UNKNOWN/PROTECTED 或未知 wire 值均阻断并要求重新生成，不猜测为低风险。
- 纯汇总：`summarizePlanImpact` 不读取 policy，按计划条目的实际 action、confirmation 和扫描风险统计对象数量、逻辑大小、永久删除/工具清理、低风险候选、本地重建、需重新下载、可能失去历史及授权要求。
- 展示：计划、预演、最终确认和结果页固定显示四类风险影响；`policy` 只保留授权门禁和策略摘要，不再决定是否显示 redownload/review 风险提示；`none` 显示“无需额外风险授权”。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1719 modules transformed`）；`node src/testing/storeProbes.mjs` 通过，输出 `12/12 batch-3 store-probe verifications passed (fixes, not defects).`；`git diff --check` 无差异错误。
- 未验证内容：浏览器真实混合计划截图、按钮焦点与 tooltip、Tauri/WebView2 原生代次竞态、真实 Windows 清理仍未验收；B01.3 已完成，B01.4 复核容器尚未实施。
- 下一步：B01.3，重整最终确认内容与按钮语义。

### B01.3：重整最终确认

- 状态：已验收（子任务）；B01 批次仍进行中。
- 修改文件：`src/stores/cleanupStore.ts`、`src/components/CleanupFlow.tsx`。
- 动态动作 CTA：纯永久删除计划显示“永久删除 N 项”；纯工具计划显示“执行工具清理”；混合计划显示“执行清理（N 项）”。动作数量仍紧邻显示在影响摘要中。
- 最终确认信息：标题/副标题包含最终动作、逻辑大小和当前授权策略；风险摘要与 redownload/review 后果说明在同一弹窗内，单独阅读最后一步即可判断对象和主要影响。
- 焦点与授权边界：修正 `ModalFrame` 让显式 `data-autofocus` 优先于普通按钮；最终确认的“取消”设为初始焦点。未新增默认勾选、未自动提升 policy、未提供撤销承诺。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1719 modules transformed`）；`node src/testing/storeProbes.mjs` 通过，输出 `12/12 batch-3 store-probe verifications passed (fixes, not defects).`。
- 未验证内容：浏览器/Tauri 实际焦点落点、最终确认单屏布局和真实 Windows 执行结果尚未验收；B01.4 复核容器与 B01.5 异步 token 已实施，B01.6 尚未实施。
- 下一步：B01.4，统一复核容器，保持摘要和按钮区域稳定。（已完成后接续 B01.5。）

### B01.4：统一计划复核容器

- 状态：已验收（子任务）；B01 批次仍进行中。
- 修改文件：`src/components/CleanupFlow.tsx`、`src/styles/global.css`。
- 稳定外框：`planning`、`plan-review`、`dry-run`、`dry-run-review` 共用同一个复核容器；计划摘要在状态切换时持续保留，中央区域按阶段显示计划表、预演忙碌态或预演结果。
- 预演语义：预演标题和说明明确“未删除文件”；同时说明工具查询等既有只读验证仍可能执行，没有把预演描述为“没有发生任何操作”。预演完成后仍进入独立的最终确认弹窗。
- 忙碌状态：规划和预演显示真实阶段文案及不定进度指示，不估造百分比或 ETA；执行态也移除原固定 `35%` 宽度，改用不定进度条。
- 关闭边界：规划/预演进行中不提供误导性的取消按钮或 Escape 关闭；等待结束后恢复显式关闭和复核操作。未新增后端取消语义。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1719 modules transformed`）；`node src/testing/storeProbes.mjs` 通过，输出 `12/12 batch-3 store-probe verifications passed (fixes, not defects).`；`git diff --check` 无差异错误（仅保留既有 CRLF 提示）。
- 未验证内容：浏览器真实多阶段切换、键盘焦点、不同窗口尺寸下的复核表滚动、Tauri/WebView2 原生等待态和真实后端延迟尚未验收；B01.5 异步 token 已实施，B01.6 失败/恢复路径尚未实施。
- 下一步：B01.5，为规划、预演和执行补充异步 token 与过期结果防护。（已完成后接续 B01.6。）

### B01.5：异步结果与执行代次隔离

- 状态：已验收（子任务）；B01 批次仍进行中。
- 修改文件：`src/stores/cleanupStore.ts`、`src/stores/generationLifecycle.ts`、`src/components/CleanupFlow.tsx`、`src/testing/storeProbes.mjs`。
- 请求身份：planning、dry-run、executing 分别保存递增 `requestToken`、原始 `scanGeneration` 和 `planId`；响应落地前同时校验 token、阶段和计划绑定，关闭/策略变化/新扫描会使旧计划或预演响应失效。
- 输入一致性：计划上下文记录原始 selection 集合和 policy；预演与最终执行前后均校验，选择集合或策略变化必须重新生成，重复点击在 store 层只允许一次请求。
- 执行边界：`executing` 与 `session` 不因新扫描调用普通 `close()`；结果继续绑定原计划与原扫描代次，结果页明确显示“计划 #… 的结果”和原扫描代次，避免把旧结果重新绑定到新条目。
- 延迟与失败：关闭后的晚到成功/失败均不会重新打开旧复核；预演等待期间输入变化也不会发布旧结果；无后端取消能力时保持等待语义，不伪装成已取消。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1719 modules transformed`）；`node src/testing/storeProbes.mjs` 通过，实际输出 `19/19 store-probe verifications passed (fixes, not defects).`；`node --check src/testing/storeProbes.mjs` 通过。
- 未验证内容：浏览器真实重复点击与键盘关闭、Tauri/WebView2 原生执行中窗口、跨进程 Busy、真实后端延迟/失败及真实清理仍未验收；B01.6 单位和失败恢复文案尚未实施。
- 下一步：B01.6，修正文案单位和恢复成本表达，再进入 B02。

### B01.6：修正文案单位和恢复成本表达

- 状态：已验收（子任务）；B01 批次完成，尚未进入 B02。
- 修改文件：`src/utils/format.ts`、`src/components/CleanupFlow.tsx`、
  `src/components/AiReviewPanel.tsx`、`src/components/ResultsTable.tsx`、
  `src/components/DetailPanel.tsx`、`src/components/common.tsx`、
  `src/pages/CategoryPage.tsx`、`src/pages/DashboardPage.tsx`、
  `src/pages/JournalPage.tsx`、`src/adapters/mockBackend.ts`。
- 风险词汇：保留 `safe` wire 枚举和 `clean --safe` 命令，用户可见标签改为“低风险候选”；不再把证据不足的条目描述成绝对“安全”。
- 大小口径：列表、详情、计划、预演、会话结果和日志统一显示“逻辑大小估计”；成功处理结果不再称“已释放”，避免把 `estimatedSize` 当作实际磁盘回收量。
- 恢复成本：本地重建文案改为“下次使用需要本地重建，耗时取决于实际使用”；联网恢复文案改为“需要联网重新下载，实际流量取决于使用内容”；确认页不再承诺固定下载量或“不会损坏工具”。
- Mock 说明：npm、pip、Cargo、uv、Rust 和 Claude 示例同步改为估算值、恢复成本和最终校验语义，避免演示数据传递确定性释放/下载/可逆承诺。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1719 modules transformed`）；`node src/testing/storeProbes.mjs` 在授权工作区通过，输出 `19/19 store-probe verifications passed (fixes, not defects).`；`git diff --check` 无差异错误，仅有仓库既有 LF/CRLF 提示。
- 未验证内容：真实浏览器截图、Tauri/WebView2 布局、真实磁盘释放量、网络流量和 Windows 清理结果仍未验证；上述文案明确保留这些测量边界。
- 下一步：进入 B02 前先由用户确认继续；B01 已完成 F01 主流程、F02 清理主流程文案和 F08 清理过程反馈，日志闭环仍待 B07，扫描反馈仍待 B06。

### B02.1：修复行与控件键盘冲突

- 状态：已验收（源码/构建）；B02 批次仍进行中。
- 修改文件：`src/components/ResultsTable.tsx`。
- 键盘边界：结果行只有在自身获得焦点（`event.target === event.currentTarget`）时才处理 Enter / Space 打开详情；checkbox 以及后续可能嵌入的 button/link 的冒泡事件保留原生行为。
- 保留语义：复选框点击仍阻止打开详情；受保护 / 未知条目继续使用 disabled checkbox；排序列仍是可操作 button，并保留 `aria-sort`。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1719 modules transformed`）；`git diff --check` 无差异错误，仅有仓库既有 LF/CRLF 提示；源码核对确认行处理器先过滤子控件目标。
- 未验证内容：按 `webapp-testing` 要求尝试启动 `with_server.py --help`，但系统 Python/Playwright 入口不可启动（WindowsApps 别名返回“指定的登录会话不存在”），未安装依赖，因此真实 DOM、checkbox Space 和行 Enter 的浏览器回归仍待可用运行时验证。
- 下一步：B02.2，抽取并完善共用 `ModalFrame` 的焦点进入、Tab 环绕、Escape 条件关闭与返回触发元素语义。

### B02.2：抽取共用 ModalFrame

- 状态：已验收（源码/构建）；B02 批次仍进行中。
- 修改文件：新增 `src/components/common/ModalFrame.tsx`；修改
  `src/components/CleanupFlow.tsx`、`src/pages/JournalPage.tsx`、`src/pages/RulesPage.tsx`。
- 统一语义：共享外框提供 `role=dialog`、`aria-modal`、标题/说明 ID 关联、`tabIndex=-1`、焦点进入和正反向 Tab 环绕；无可聚焦控件时焦点落到外框。
- 关闭边界：Escape 只在 `dismissible` 时调用关闭；不可关闭的执行中/删除中/清空中状态会消费 Escape，避免事件穿透到底层页面；遮罩不绑定背景点击关闭。
- 背景隔离：ModalFrame 对同一根节点下的背景兄弟节点设置真实 `inert`，卸载时恢复原状态；关闭后优先恢复原触发元素，触发元素已卸载时回退到当前页面标题/合理入口。
- 初始焦点：计划复核显式指定“先预演/返回调整”或“关闭”；最终危险清理、规则删除和日志清空显式聚焦“取消”；执行中无危险按钮抢焦点，保持不可关闭。
- 迁移范围：清理计划/预演/确认/执行/结果/错误、日志清空和规则删除均复用共享组件；规则删除不再使用 `window.confirm`。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1720 modules transformed`）；`node src/testing/storeProbes.mjs` 通过，输出 `19/19 store-probe verifications passed (fixes, not defects).`；`git diff --check` 无差异错误，仅有仓库既有 LF/CRLF 提示；静态检索未发现 `window.confirm` 或裸 `modal-veil` 调用方。
- 未验证内容：真实浏览器焦点、inert/WebView2 行为和屏幕阅读器输出仍待可用 Python/Playwright 运行时；未为此安装依赖。共享组件已保留真实运行时验证入口。
- 下一步：B02.3，补齐表单控件、导航、筛选和页面标题的可访问语义。

### B02.3：补齐表单、导航、筛选和页面语义

- 状态：已验收（源码/构建）；B02 批次仍进行中。
- 修改文件：`src/components/AppShell.tsx`、`src/components/Sidebar.tsx`、
  `src/components/ScanBar.tsx`、`src/components/AiProfilePanel.tsx`、
  `src/components/AiReviewPanel.tsx`、`src/pages/DashboardPage.tsx`、
  `src/pages/CategoryPage.tsx`、`src/pages/AiReviewPage.tsx`、
  `src/pages/UnknownPage.tsx`、`src/pages/RulesPage.tsx`、
  `src/pages/JournalPage.tsx`、`src/pages/SettingsPage.tsx`、
  `src/styles/global.css`。
- 页面骨架：应用工作区改为 `main`；概览、分类、未知数据、AI 研判、规则、日志和设置均有页面级 `h1`，空态提前返回也保留标题上下文。
- 导航状态：主导航增加 `aria-label` 和当前页 `aria-current="page"`；主题切换图标按钮增加动态可读名称；主题和分类子筛选按钮使用 `aria-pressed`，未引入不完整的 ARIA tabs 模式。
- 表单标签：扫描范围、分类搜索/风险筛选、日志会话/结果、规则搜索、工作区根目录和 AI 配置字段均有稳定 ID 与可见或辅助标签；AI 主开关、活动配置、名称、Base URL、模型、API Key、超时、接口模式、结构化输出和启用状态均完成显式关联。
- 错误关联：AI 配置和 AI 研判错误区域有稳定 ID，并通过 `aria-describedby` 关联到对应区域/表单；动态建议中的最终风险、最终类别和候选复选框具备可定位 ID。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1720 modules transformed`）；`node src/testing/storeProbes.mjs` 通过，输出 `19/19 store-probe verifications passed (fixes, not defects).`；`git diff --check` 无差异错误，仅有仓库既有 LF/CRLF 提示。
- 未验证内容：真实可访问性树、键盘/屏幕阅读器、WebView2 对 `main`/`aria-current`/`aria-pressed` 的输出仍待可用浏览器运行时验证；当前机器 Python/Playwright 入口不可启动，未安装依赖。
- 下一步：B02.4，统一 ScanBar 警告展开、状态播报和忙碌区域 `aria-busy` 语义。

### B02.4：统一 ScanBar 状态反馈

- 状态：已验收（源码/构建/探针）；B02 批次仍进行中。
- 修改文件：`src/components/ScanBar.tsx`、`src/styles/global.css`。
- 警告展开：扫描警告标题改为原生 `button`，补充 `aria-expanded` 与 `aria-controls`，警告列表使用稳定 ID；保留可见焦点和原有展开视觉。
- 状态播报：扫描进行中使用 `role="status"` 与 `aria-live="polite"`，仅播报 provider/stage 阶段文本；逐项变化的“已发现 N 项”作为视觉细节置于 `aria-hidden`，避免高频逐条播报。完成/取消状态同样以 polite status 呈现，扫描失败使用 `role="alert"`。
- 忙碌语义：扫描栏在扫描期间设置 `aria-busy="true"`；不再仅依靠脉冲进度条表达忙碌状态，进度装饰本身标记为 `aria-hidden`。
- 其他细节：扫描控制按钮补充 `type="button"`；取消和代次变化提示使用 status 语义，图标标记为装饰性内容。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1720 modules transformed`）；`node src/testing/storeProbes.mjs` 在提升权限的临时缓存目录下通过，输出 `19/19 store-probe verifications passed (fixes, not defects).`；`git diff --check` 未发现新增差异错误，仅有仓库既有 LF/CRLF 提示。
- 未验证内容：真实可访问性树、屏幕阅读器对 live region 的实际去重行为、键盘焦点和 Tauri/WebView2 渲染仍待可用浏览器运行时验证；当前机器 Python/Playwright 入口不可启动，未安装依赖。
- 下一步：B02.5，验证共用规则与完整键盘主流程（扫描结果 → 勾选 → 详情 → 计划 → 预演 → 取消确认）。

### B02.5：验证共用规则与详情焦点闭环

- 状态：已验收（源码/构建/探针）；B02 批次完成。
- 修改文件：`src/components/ResultsTable.tsx`、`src/components/DetailPanel.tsx`。
- 详情焦点：结果行增加稳定 `data-detail-trigger` 标识；详情挂载或切换条目时关闭按钮获得焦点；关闭详情后回到原触发行，若条目已被筛选或新快照移除则不聚焦失效节点。
- 键盘边界：保留 B02.1 的行自身 Enter/Space 行为，子控件仍使用原生键盘语义；关闭按钮声明 `type="button"`，不触发表单提交。
- 验证命令：`npm run build` 通过（`tsc --noEmit`、Vite `1720 modules transformed`）；`node src/testing/storeProbes.mjs` 在提升权限的临时缓存目录下通过，输出 `19/19 store-probe verifications passed (fixes, not defects).`。
- 未验证内容：真实浏览器的正反 Tab、checkbox Space、详情焦点转移、模态焦点边界和屏幕阅读器输出仍待可用 Python/Playwright 或 Tauri/WebView2；当前环境无法启动 Python/Playwright，未安装依赖。完整“扫描结果 → 勾选 → 详情 → 计划 → 预演 → 取消确认”仅完成源码路径核对，未标记为浏览器验收通过。
- 下一步：B03.1，固定清理记录与扫描/计划重置的后端接口范围。

### B03.1：固定清理记录与扫描/计划重置接口

- 状态：实现完成待 B03 其余子任务；B03 批次进行中。
- 修改文件：`src/types/index.ts`、`src/adapters/backend.ts`、`src/adapters/tauriBackend.ts`、`src/adapters/index.ts`、`src/adapters/mockBackend.ts`、`src/pages/JournalPage.tsx`、`src/pages/SettingsPage.tsx`、`src/stores/unknownWorkflowStore.ts`、`src-tauri/src/contract.rs`、`src-tauri/src/journal.rs`、`src-tauri/src/main.rs`、`src-tauri/src/ipc_contract.rs`。
- 新接口：新增 `clearJournal()` / `clear_journal`（返回 `removedShardCount`）与 `resetScanData()` / `reset_scan_data`（返回 `removedPlanCount`、`hadSnapshot`）；旧 `clearAllData()` / `clear_all_data` 保留兼容，不再由新 UI 调用。
- UI 范围：日志页改为“清除清理记录”，只刷新 journal；设置页新增“应用数据 → 重置扫描与计划”，成功后清理扫描、选择、计划、AI 批次和未知工作流临时状态，保留日志、规则、AI 配置与偏好。
- Mock 语义：重置扫描/计划不重置 `nextPlanId`、`nextGeneration`、journal、dispositions 或 AI profile metadata；清日志不影响 snapshot/plans。
- 验证：`npm run build` 通过（`1720 modules transformed`）；Tauri `cargo test --manifest-path src-tauri/Cargo.toml --features custom-protocol` 通过（52/52，使用新的 `target-tauri-local` 避开旧 D 盘构建缓存）。
- 未完成/未验证：B03.2 的跨进程应用记录锁、generation 水位重置细节、真实临时数据根维护尚未完成；浏览器弹窗与真实 Tauri IPC 运行时尚未验收。
- 下一步：B03.2，收紧 Core 日志/PlanStore 清理范围并保留编号水位。

### B03.2：受限日志与计划存储维护

- 状态：实现完成待 B03 其余子任务；B03 批次进行中。
- 修改文件：`crates/devresidue-core/src/journal/mod.rs`、`crates/devresidue-core/src/cleanup/plan_store.rs`、`src-tauri/src/journal.rs`。
- 安全范围：日志清理仅匹配 `devresidue-YYYYMMDD.jsonl` 或带数字分片后缀的合法文件，目录、未知文件和 reparse/symlink 条目保留；PlanStore 新增 `clear_plans()`，只删除数字 ID 的 `.json`，保留 `_next_id` 与锁文件。
- 部分失败：计划删除错误包含已删除数量和失败目标；快照删除与计划清理按顺序执行，失败时不伪装全部成功。
- 验证：`cargo test -p devresidue-core` 通过（261/261 单元测试、2/2 文档测试）；Tauri 52/52 通过。
- 未完成/未验证：持久 generation 水位的重置迁移与跨进程应用操作锁待 B03.3；reparse/权限矩阵仍未在真实 Windows 临时数据根验收。
- 下一步：B03.3，补齐操作生命周期 guard 与 Busy 互斥。

### B03.3：操作生命周期 guard 与 Busy 错误

- 状态：部分实现；B03 批次进行中。
- 修改文件：`src/types/index.ts`、`src-tauri/src/contract.rs`、`src-tauri/src/state.rs`、`src-tauri/src/scan.rs`、`src-tauri/src/plan.rs`、`src-tauri/src/journal.rs`、`src-tauri/src/disposition.rs`、`src-tauri/src/rules.rs`、`src-tauri/src/ai.rs`。
- 已实现：进程内 `AppState` 操作登记与 RAII guard；扫描 worker、计划生成、预演/执行、AI prepare/analyze/confirm、规则删除、未知处置和维护命令保持完整生命周期；冲突返回结构化 `busy`，不依赖 React 禁用按钮。
- 已实现：扫描/执行异步 guard 分别覆盖 worker 和 `spawn_blocking`；写入失败/异常退出会通过 guard 自动释放。
- 未完成：GUI 与 CLI 共享数据根的 `app-data-operation.lock` 跨进程文件锁尚未接入；AI 远程等待期间的跨进程锁策略和相关 CLI 写入口尚未全量覆盖，不能标记 UI-032/UI-035 通过。
- 验证：Tauri `cargo test --manifest-path src-tauri/Cargo.toml --features custom-protocol` 52/52；`npm run build` 通过。

### B03.4：部分失败与权威状态反馈

- 状态：已实现（后端/前端）；真实权限故障矩阵未验证。
- 维护错误：日志分片清理和计划清理均返回实际已完成数量；`PlanStore::clear_plans` 的失败信息包含已删除数量与失败目标，不把部分完成包装成事务回滚或“全部成功”。
- 重置顺序：`reset_scan_data` 先移除旧扫描快照使旧对象失效，再在锁内清理合法计划记录；任一步失败都返回错误并保留具体原因，不自动重试破坏性维护。
- UI：日志页与设置页保留维护错误区域，不把失败状态切换成理想空态；成功后才刷新对应的权威状态。
- 未验证：需要独立临时数据目录注入 Reparse、权限拒绝和“删除若干分片后失败”的 Windows 矩阵；当前未调用真实用户数据目录。

### B03.5：维护入口与确认范围

- 状态：已验收（源码/构建/Tauri 测试）。
- 日志入口为“清除清理记录”，确认框明确只删除 DevResidue 日志分片，并保留扫描、计划、规则、AI 配置、偏好、编号水位及用户文件。
- 设置入口为“应用数据 → 重置扫描与计划”，确认框明确清除扫描快照、计划和依附的临时选择/AI 批次，保留日志、规则、AI 配置、偏好、编号水位，且不删除项目或缓存文件。
- 两个入口使用 `ModalFrame`，取消为默认焦点，执行期间禁止重复提交与 Escape 关闭；后端 Busy 会以结构化命令错误返回。

### B03.6：前端状态同步

- 状态：已验收（源码/构建/探针）。
- 仅清日志成功后只重新读取 journal，不清 scan、selection、cleanup plan 或 AI 配置。
- 重置扫描与计划成功后清除扫描视图、全局选择、清理复核、详情、未知处置临时状态和 AI 批次，并回到概览；设置偏好与服务配置保持不变。
- 新扫描代次继续通过 generation lifecycle 使旧选择、计划和 AI 批次失效；MockBackend 的清理范围和返回字段与 Tauri DTO 对齐。
- 未验证：维护失败后真实后端计划列表的重新读取仍受当前 IPC 只读接口范围限制；浏览器/Tauri 原生弹窗与跨窗口 Busy 尚未验收。

### B04.1：统一列表/详情工作区

- 状态：已实现（源码/构建）；真实窗口截图未验证。
- 分类、风险结果和已选清单统一使用“页头/工具栏 → 可滚动列表 → 独立全局操作栏”的 flex 容器；详情作为右侧证据列，窄窗口转为列表区内的覆盖面板，不再依赖增加 z-index 掩盖操作栏。
- 结果表保留名称与位置、逻辑大小估计、影响、勾选和详情入口；次要证据仍在详情面板，长路径换行/省略由 CSS 处理。

### B04.2：查看详情与勾选状态分离

- 状态：已实现（源码/构建）。
- `detail-open` 与 `selected` 使用独立样式；打开详情不会自动加入选择，UNKNOWN/PROTECTED 继续禁用普通勾选。
- 详情关闭时通过 `data-detail-trigger` 返回原结果行焦点，条目已不可见时不聚焦失效节点。
- 未验证：960×640 下 WebView2 实际滚动位置和窗口截图仍待原生环境。

### B04.3：跨页选择并集/差集

- 状态：已验收（源码/探针）。
- `selectionStore` 新增 `addMany` / `removeMany`；当前视图全选把可选 ID 并入全局集合，取消全选只移除当前视图可选 ID，`clear` 仍仅用于明确清空全部选择。
- 表头 checkbox 根据当前视图的可选集合计算 checked/indeterminate；UNKNOWN/PROTECTED 永远不进入可选集合。
- 验证：`npm run build`；`node src/testing/storeProbes.mjs` 输出 `19/19 store-probe verifications passed`。

### B04.4：全局操作栏与已选清单

- 状态：已实现（源码/构建）。
- 新增辅助页面 `selected-items`，展示当前代次的全局已选对象、数量和逻辑大小估计，复用结果表支持逐项取消勾选、打开详情和返回概览；失效 ID 自动移除，不按路径重新匹配。
- 操作栏提供“查看已选”和“清空全部选择”；清理计划入口只在 AI Agents、开发缓存、项目、风险结果和已选清单显示，设置/规则/日志/AI/未知页只显示轻提示。
- 未验证：真实跨页面点击和滚动位置需浏览器/Tauri 原生验收。

### B04.5：结果页操作入口约束

- 状态：已实现（源码/构建）。
- `CleanupFlow` 根据当前页面决定是否渲染确认级别选择器与“生成清理计划”；非结果页不出现看不到对象的执行按钮，避免把全局选择误当作当前页面对象。

### B04.6：代次隔离与回归

- 状态：已实现（源码/探针）；完整 UI 验收未完成。
- 搜索、排序、分类切换和进入已选清单不清空全局集合；新扫描、重置和 generation lifecycle 仍清除旧代次 ID，避免数字 ID 重用误选。
- 验证：前端生产构建通过（`1721 modules transformed`），CLI `cargo test -p devresidue-cli --locked` 27/27，Tauri 52/52。
- 未验证：真实 960×640、1200×900、1440×900 的键盘/截图验收；当前 Python/Playwright 运行时不可用。
- 下一步：B03.4—B03.6，完善部分失败反馈、确认文案和前端权威状态刷新；之后进入 B04。

### B05.1：统一分类与统计 selector

- 状态：已实现（源码/构建/探针）；真实窗口截图未验证。
- 新增 `src/selectors/scanPresentation.ts`，集中定义 `familyOfItem`、`itemsForFamily`、`familySummary`、`riskSummary`；归属只依据 `source`/明确 provider 语义，风险不改变主归属。
- Sidebar、AppShell、Dashboard、CategoryPage、UnknownPage 统一使用 selector；Agent 数据、工具数据、项目产物和待判断的数量口径不再各自猜测。
- 验证：`npm run build` 通过（`1722 modules transformed`）；`node src/testing/storeProbes.mjs` 19/19。

### B05.2：导航、待判断双子页与筛选统计

- 状态：已实现（源码/构建）；浏览器键盘与真实截图未验证。
- 内部 `PageKey` 保持兼容，新增 `selected-items` 辅助页；导航文案改为 Agent 数据、工具数据、项目产物、待判断，待判断拆分“未知数据 / 需人工确认”。
- UnknownPage 支持搜索、子页切换、筛选结果数/总数；普通清理入口不会出现在未知/规则/日志/设置等不适用页面。
- 未验证：真实跨页点击、窄窗口滚动和 WebView2 渲染仍受当前机器无可用 Playwright 运行时影响。

### B05.3：项目与工作区归属

- 状态：已实现（selector/现有证据字段复用）。
- 项目条目继续由 `kondo` source 归入项目产物，Agent provider 不因路径字符串误归类；风险与主归属独立。
- 未新增 Core 字段或改变授权快照格式；真实 Kondo 嵌套根矩阵待 B11 隔离工作区验收。

### B05.4：概览统计入口

- 状态：已实现（源码/构建）。Dashboard 成本卡片改为 family selector 汇总，展示数量、逻辑大小估计及可点击入口；零值与比例计算不除零。
- 验证：前端构建通过；真实图表键盘等价入口和不同窗口布局待 B10/B11。

### B05.5：边界数据与选择资格

- 状态：已实现基础边界（源码/探针）。全局选择仍由风险资格与 generation 控制，分类切换不自动改变选择资格；UNKNOWN/PROTECTED 不进入普通选择。
- 未验证：包含 Agent 临时文件、SSH 凭据、同名不同路径项目的大规模真实 fixture 矩阵待 B11。

### B06.1：默认扫描范围模式

- 状态：已实现（前端/Rust/构建/测试）。
- `workspaceRootsMode=automatic|custom` 已加入 settings v2；`default` scope 省略 `workspace_roots` 表示自动解析，显式数组表示自定义，空数组跳过项目 provider；Projects scope 仍要求显式 roots。
- Rust `scope_plan`、MockBackend、ScanBar、Dashboard 使用同一语义；旧 v1 根目录偏好迁移为待处理草稿，不静默改变自动模式。

### B06.2：只读范围校验、预览与应用数据接口

- 状态：已实现（真实/Mock adapter、Tauri IPC、构建/测试）。
- 新增 `pick_workspace_directory`、`validate_workspace_roots`、`get_scan_scope_preview`、`get_app_data_info`，前端 Backend 与 lazy adapter 已同步。
- 校验覆盖空白、绝对路径、重复、嵌套、存在性与目录类型；预览不读文件内容、不创建计划；应用数据目录从真实 AppState 返回。
- 当前无 dialog 插件依赖，原生选择命令安全返回 `None`，手工输入仍为权威入口；不将此项写成“原生选择已验收”。

### B06.3：范围解析与路径边界

- 状态：基础实现完成；真实 Windows Reparse/权限矩阵未验证。
- 扫描与预览共享 `ScanScope` 语义；路径比较使用组件边界，盘符根和 UNC 不会被简单尾斜杠裁成盘符字符串。
- 未验证：真实无权限、Reparse、大小写重复和嵌套根在 Windows 临时数据根中的行为待 B11。

### B06.4：设置保存与启动衔接

- 状态：已实现基础状态衔接；保存 UI 与真实运行时仍待验收。
- 设置页区分自动/自定义模式，保存根目录即持久化模式；默认扫描入口统一调用 `defaultScope`。快照的真实 roots 仍以后端 `ScanMode::Real` 为权威，不用当前设置冒充历史范围。

### B06.5：首次与空态

- 状态：已实现部分（源码/构建）。首次/空结果入口已区分概览、分类和待判断页面，保留扫描动作与范围提示；完整“未扫描/无匹配/过滤为空/失败/部分取消/过期”文案矩阵待后续收敛。

### B06.6：全局扫描状态

- 状态：基础能力已实现（既有 ScanBar + 构建/探针）。扫描范围、provider 阶段、发现数量、警告和取消终态继续由全局状态驱动；无真实完成比例时保持不定进度。
- 未验证：真实取消延迟、权限失败影响范围和 WebView2 全局布局待 B11；重试不改变既有授权范围。

### B07.1：会话优先的清理记录

- 状态：已实现（源码/构建/探针）；真实日志文件和长列表截图未验证。
- JournalPage 按 `sessionId` 分组展示时间、记录数、成功/跳过/失败/未记录数量和逻辑大小估计；组内表格只保留对象与位置、操作、结果、大小、原因摘要和详情入口。
- 详情保留阶段、完整路径、原始动作、原始结果和原始错误，可复制纯文本；主表中的路径做截断，搜索仍使用完整值。

### B07.2：失败定位与搜索

- 状态：已实现（源码/构建）。
- 结果筛选新增“结果未记录”，搜索覆盖已加载的产品、路径、原因、动作、阶段和结果，并明确当前仅搜索最近加载的 200 条记录。
- 原因摘要继续使用 `cleanupPresentation` 的中文映射；未知错误保留原文详情，不自动提权、不自动杀进程、不提供绕过既有计划流程的重试。

### B07.3：规则阅读筛选

- 状态：已实现（源码/构建）。
- RulesPage 改为共享真实规则 store，提供全部/用户/内置来源筛选和全部/有问题校验筛选；文件级校验问题仍在状态条显示，规则级问题在行内显示。
- 行内 `details` 展示 ruleId、来源、类别、风险、校验状态和问题原文；当前接口未返回匹配条件，因此明确提示“不猜测规则命中范围”。

### B07.4：真实规则关联

- 状态：已实现（源码/构建）。
- `ScanItemDto` 新增 `classification_rule_id`；DetailPanel 使用该字符串与真实 `RuleDto.ruleId` 精确匹配，移除静态 `RULES` 演示表依赖。
- 数字 `Evidence.rule_id` 不再强制转换为规则 slug；无可靠关联时展示原始规则 ID/数字证据和“未提供可跳转规则”，不伪造命中。

### B07.5：用户规则删除闭环

- 状态：已实现（已有后端接口 + 本批真实 store/构建）。
- 仅 `user` / `user-protected` 来源显示删除按钮，内置规则显示不可删除；删除前说明只影响后续扫描，成功后重新读取规则和校验结果。
- 未验证：真实用户规则文件权限失败、删除后实际重扫分类变化和 Tauri 原生确认框待 B11 隔离数据根验收。

### B08.1：人工判断与 AI 入口

- 状态：已实现（源码/构建）。
- 待判断详情保留打开位置、忽略、保护动作，并新增“使用 AI 辅助判断”次级入口；该入口只携带当前 item ID 和 generation，不自动发送请求。
- UnknownPage 搜索后详情只引用当前可见集合，不会把隐藏条目重新展示。

### B08.2：AI 三步结构

- 状态：已实现（源码/构建/探针）。
- AiReviewPanel 显式显示“选择条目 → 确认发送内容 → 复核建议”步骤条；远程请求仍只由“发送脱敏元数据并分析”按钮触发。
- 单条目入口会预选该条目；普通入口仍可全选当前 Unknown/Review 候选。

### B08.3：有限返回上下文与代次隔离

- 状态：已实现（内存 store/构建）。
- `aiStore.returnContext` 只保存来源子页、候选 item IDs 与 generation，不保存路径、Key 或请求体；返回时恢复来源子页。
- generation 变化会清除上下文并提示重新选择，过期 ID 不会绑定到新扫描。

### B08.4：发送内容与路径提示

- 状态：已有协议/本批复核通过。
- 默认 prepare 只展示脱敏元数据；勾选完整路径后预览、说明和按钮持续明确“包含完整路径”，仍不发送文件内容、Key、凭据或原始证据。
- 未验证：真实联网服务和 WebView2 滚动/按钮冻结待 B11；Mock 流程可由 store probes 覆盖基础状态。

### B08.5：建议复核与规则保存

- 状态：已实现（源码/构建）。
- 建议默认不选，最终风险/类别逐项选择；低风险会触发额外提醒。按钮改为“保存为用户规则”，成功提示明确不创建清理计划、不删除数据。
- 规则保存仍由后端枚举和本地校验闭合，之后继续走原清理计划流程。

### B08.6：异常链路

- 状态：已有错误闭环；真实服务矩阵未验证。
- store 会分别保留配置、准备、请求、取消、确认错误；失败不会复用上一批 suggestions，代次变化会清理批次。配置/模型接口继续有独立忙碌态。
- 未验证：关闭服务、认证失败、超时、非法格式、保存失败等真实后端场景待 B11 专用配置；未使用真实用户凭据。

### B09.1：设置五组

- 状态：已实现（源码/构建）。
- SettingsPage 顺序固定为“扫描位置 → 外观 → AI 服务 → 应用数据 → 关于”，各组保留必要说明和稳定标题；维护入口仍在应用数据组。

### B09.2：扫描位置保存与校验

- 状态：已实现基础能力（前端/真实与 Mock 接口/构建）。
- 添加根目录前调用 `validateWorkspaceRoots`，逐项显示空白、相对、重复、嵌套、不存在/非目录等问题；失败保留草稿，成功后才写入 settings v2。
- 提供“选择文件夹”入口；当前无 dialog 插件时明确降级为手工输入，不伪称已选择。

### B09.3：外观持久化

- 状态：已实现（源码/构建）。
- `uiStore` 独占 `devresidue.appearance.v1`，启动校验 system/dark/light，跟随系统模式继续由 App 监听系统主题；settingsStore 不复制 theme。

### B09.4：AI 配置按需组织

- 状态：已有按配置列表/编辑表单结构；本批调整了组顺序和错误/忙碌保留。
- API Key 仍是局部 uncontrolled password input，保存、失败、取消后清空；测试连接与获取模型各自按钮和忙碌状态，不自动发送扫描数据。

### B09.5：表单保存反馈

- 状态：已实现（现有 AI 面板/构建）。
- 字段 label、错误关联、超时范围、接口模式和结构化输出已保留；保存成功显示非敏感状态，失败不清空非敏感草稿，Key 始终清空。

### B09.6：应用数据与关于

- 状态：已实现基础能力（真实/Mock adapter、Tauri IPC、构建）。
- 设置页通过 `getAppDataInfo` 展示实际数据目录和 backend 模式；读取失败显示失败，不回退固定路径冒充事实。
- 未验证：真实发布包数据目录、诊断复制白名单和 Windows 原生窗口待 B11。

### B10.1：视觉 token 与页面滚动边界

- 状态：已实现基础收敛（源码/构建）。
- global.css 增加 spacing、small radius、focus ring 和 motion token；AI 研判页明确由 `page-body` 承担唯一纵向滚动，避免列表卡片与页面同时滚动。
- 未验证：真实 WebView2 双滚动截图待 B11；大范围旧 CSS 重复覆盖仍需逐页核对。

### B10.2：统一页面框架与窄窗布局

- 状态：已实现基础收敛（源码/构建）。
- 设置页按扫描位置、外观、AI 服务、应用数据、关于分组；结果/日志/规则保留稳定页头与内容区；窄窗口 AI 表单切为单列。
- 未验证：960/1200/1440 CSS viewport 下的真实截图和滚动焦点待 B11。

### B10.3：列表、详情与记录密度

- 状态：已实现（B04/B07 叠加验证）。
- 结果表保留名称/位置/大小/影响，详情承载完整路径与证据；日志主表路径截断，原文详情可展开复制；规则详情不伪造未返回的匹配条件。

### B10.4：颜色、焦点与强制颜色

- 状态：已实现基础规则（源码/构建）。
- 深浅主题保留语义 token；增加 forced-colors 下焦点和徽标边界，保护风险仍与失败按钮颜色分离。
- 未验证：真实系统对比度测量、屏幕阅读器和强制颜色模式待 B11。

### B10.5：响应式与环境偏好

- 状态：已实现基础规则（源码/构建）。
- `prefers-reduced-motion` 会削弱转场/扫描动画；AI 页和设置表单在窄窗口不产生额外横向滚动；主题 system/dark/light 已独立持久化。
- 未验证：100%/125%/150%/200% DPI 的原生窗口矩阵待 B11。

### B10.6：大列表性能决策

- 状态：未宣称完成，待 B11 实测。
- 当前保持完整结果集渲染，确保全选语义仍覆盖全部筛选结果；未在没有 1,000/10,000 条实测前引入分页或虚拟列表。

### B11.1：workspace 回归

- 状态：已通过可执行回归。
- `cargo test --workspace --locked` 在独立 `target-workspace-local` 下完成；核心、providers、平台文件系统、CLI 相关测试及 doc-tests 均通过，未见失败用例。

### B11.2：Tauri/CLI/前端回归

- 状态：已通过可执行回归。
- `cargo test --manifest-path src-tauri/Cargo.toml --features custom-protocol --locked`：52 passed；`cargo test -p devresidue-cli --locked`：27 passed；`npm run build`：Vite 构建成功（1,723 modules transformed）。
- `node src/testing/storeProbes.mjs`：20/20 probes passed（新增高频扫描条目批处理回归）。

### B11.3：安全边界复核

- 状态：已通过源码和测试复核。
- AI 发送仍只使用脱敏元数据，路径按显式选项加入；未发送文件内容、API Key、凭据或原始证据。AI 建议继续经过本地校验后才保存为用户规则，不创建清理计划、不删除数据。

### B11.4：未能在当前环境执行的验收

- 状态：未验证，不能标为通过。
- 当前环境无法启动 Playwright/WebView2 浏览器会话，因此 960/1200/1440 viewport、双滚动截图、键盘焦点、屏幕阅读器、DPI 和真实发布包数据目录仍需在 Windows 桌面环境手工验收。
- 原生“选择文件夹”在未引入 dialog 插件时保留安全降级：按钮不伪造选择结果，用户可手工输入并通过后端校验。

### B11.5：收尾与后续

- 状态：本批实现收尾。
- 未执行任何 Git 写操作；本地回归生成的 `target-cli-local`、`target-tauri-local`、`target-workspace-local` 仅为构建产物，是否清理或加入忽略规则由仓库维护者决定。
- 后续优先级：在真实 WebView2 环境执行 UI 验收；准备 1,000/10,000 条结果集测量后再决定是否引入虚拟列表。

### B11.6：关于信息与扫描流畅度收敛

- 状态：已实现并通过源码、探针与前端构建验证。
- 关于页去除 M4/Phase、Rust/Tauri 外壳、CleanupEngine 等内部实现信息，改为展示版本、产品名称、上次扫描和数据安全说明。
- `scanStore` 将高频 `scan://item` 事件按 50ms 窗口批量发布，扫描完成、失败、重置和新 epoch 切换前都会冲刷或清理待发布条目；进度事件历史限制为最近 64 条。
- 进度条动画使用合成层变换；减少动效偏好下改为稳定的静态活动状态，避免动画被浏览器压缩后看起来卡住。
- 验证：`node src/testing/storeProbes.mjs` 20/20；`npm run build` 成功（1,723 modules transformed）。
- 未验证：真实 WebView2 下的视觉帧率与长列表渲染仍需桌面环境手工验收。

### B11.7：分类页筛选栏换行收敛

- 状态：已实现并通过前端构建验证。
- 工具数据、Agent 数据和项目产物的筛选栏始终保持单行；工具数据分类标签和风险筛选文案已压缩，避免单个下拉控件孤零零掉到第二行。
- 极窄窗口保留单一横向滚动面，不拆分筛选控件，不影响筛选和结果列表。
- 验证：`npm run build` 成功（1,723 modules transformed）。
