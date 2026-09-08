# resources/rules/agents

Rule files in this directory describe the **internal layout of AI coding agent
data directories** (Codex, Claude Code, OpenCode, Cursor, Windsurf, ...).

Purpose (see SPEC §9 and PLAN Phase 9):

- Split each agent's data into fine-grained items:
  `Cache / Logs / Temp` (low risk) vs `Sessions / History / Workspace State`
  (Review) vs `Config / Auth / Credentials` (Protected).
- The **whole** `~/.codex`, `~/.claude`, `~/.opencode`, ... root directory must
  never be classified as a single SAFE item.

Rule files are **not implemented yet** — they arrive with the Rule Engine in
Phase 3. Until then this directory intentionally contains only this README.

Rule id convention (decided in Phase 3): `<category>/<product>/<kind>` slugs
registered by the rule loader, e.g. `agents/claude-code/cache`.
