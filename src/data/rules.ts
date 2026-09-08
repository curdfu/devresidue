import type { RiskLevel, RuleDto, RulesValidationDto } from "@/types";

/**
 * Demo rules registry (mock-backend data source, R11). Mirrors
 * `resources/rules/*.yaml` shapes; the mock's `getRules`/`validateRules`
 * serve these so the Rules page works in browser dev. The real backend
 * serves the actual merged rule set via `get_rules`/`validate_rules` —
 * this module is explicitly demo-only and never used when the Tauri
 * backend is live.
 */
export interface RuleInfo {
  id: number;
  slug: string;
  source: string;
  risk: RiskLevel;
  category: string;
  description: string;
  match: string;
  valid: boolean;
}

export const RULES: RuleInfo[] = [
  {
    id: 2,
    slug: "builtin-protected/ssh",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "SSH keys, config and known_hosts",
    match: "exact: %USERPROFILE%/.ssh",
    valid: true,
  },
  {
    id: 3,
    slug: "builtin-protected/gnupg",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "GPG keyrings and trust database",
    match: "exact: %USERPROFILE%/.gnupg",
    valid: true,
  },
  {
    id: 4,
    slug: "builtin-protected/aws",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "AWS credentials and config",
    match: "exact: %USERPROFILE%/.aws",
    valid: true,
  },
  {
    id: 5,
    slug: "builtin-detection/node-project-node-modules",
    source: "builtin-detection",
    risk: "regenerable-download",
    category: "dependency",
    description: "npm project dependency tree, re-downloadable via npm ci",
    match: "glob: %USERPROFILE%/repos/*/node_modules + parent_marker: package.json",
    valid: true,
  },
  {
    id: 12,
    slug: "builtin-detection/claude-shell-snapshots-cache",
    source: "builtin-detection",
    risk: "safe",
    category: "ai-agent",
    description: "Claude Code shell-snapshot cache (regenerated on demand)",
    match: "exact: %USERPROFILE%/.claude/shell-snapshots",
    valid: true,
  },
  {
    id: 13,
    slug: "builtin-detection/claude-session-history",
    source: "builtin-detection",
    risk: "review",
    category: "session",
    description: "Claude Code per-project session transcripts (review before cleanup)",
    match: "glob: %USERPROFILE%/.claude/projects/*",
    valid: true,
  },
  {
    id: 14,
    slug: "builtin-detection/claude-project-trash-cache",
    source: "builtin-detection",
    risk: "safe",
    category: "temporary",
    description: "Claude Code trash/interrupt caches under session projects",
    match: "glob: %USERPROFILE%/.claude/projects/*/.trash/**",
    valid: true,
  },
  {
    id: 20,
    slug: "builtin-detection/codex-temp",
    source: "builtin-detection",
    risk: "safe",
    category: "temporary",
    description: "Codex temporary working files (regenerated on demand)",
    match: "exact: %USERPROFILE%/.codex/.tmp",
    valid: true,
  },
  {
    id: 21,
    slug: "builtin-detection/codex-session-history",
    source: "builtin-detection",
    risk: "review",
    category: "session",
    description: "Codex conversation session history (review before cleanup)",
    match: "glob: %USERPROFILE%/.codex/sessions/**",
    valid: true,
  },
  {
    id: 22,
    slug: "builtin-detection/codex-computer-use-cache",
    source: "builtin-detection",
    risk: "review",
    category: "session",
    description: "Codex computer-use screen captures — may contain sensitive content",
    match: "glob: %USERPROFILE%/.codex/cache/computer-use/**",
    valid: true,
  },
  {
    id: 31,
    slug: "builtin-protected/opencode-config",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "OpenCode config directory (auth, credentials, user config)",
    match: "glob: %USERPROFILE%/.config/opencode/**",
    valid: true,
  },
  {
    id: 33,
    slug: "builtin-detection/claude-agent-cache-narrowed",
    source: "builtin-detection",
    risk: "safe",
    category: "ai-agent",
    description: "Cache sub-layout of the agent dir, narrowed by include",
    match: "glob: %USERPROFILE%/.claude/* + include: shell-snapshots/**",
    valid: true,
  },
];

export interface RuleValidation {
  total: number;
  valid: number;
  checkedAt: string;
}

export function ruleValidation(): RuleValidation {
  const valid = RULES.filter((r) => r.valid).length;
  return {
    total: RULES.length,
    valid,
    checkedAt: new Date().toLocaleTimeString("en-US", { timeStyle: "short" }),
  };
}

// ---- Mock-backend mapping (R11) ----------------------------------------------

/**
 * One deliberately invalid user rule in the demo set: proves the Rules page
 * renders non-green validation states (review R11 — "injected invalid rules
 * must not show all-green validated"). Mirrors the backend semantics: a
 * rule whose declaration failed validation is returned with `valid: false`
 * and null metadata (source/risk/category did not load).
 */
const DEMO_INVALID_USER_RULE: RuleDto = {
  ruleId: "user/protect-all-of-c",
  source: null,
  risk: null,
  category: null,
  description: null,
  valid: false,
  issues: ["match path C:\\ resolves to a drive root — rejected by path-bound validation"],
};

/** Maps the demo registry onto the R11 wire DTO for the mock backend. */
export function mockRuleDtos(): RuleDto[] {
  return [
    ...RULES.map((r): RuleDto => ({
      ruleId: r.slug,
      source: r.source,
      risk: r.risk,
      category: r.category,
      description: r.description,
      valid: r.valid,
      issues: [],
    })),
    DEMO_INVALID_USER_RULE,
  ];
}

/** Demo validation aggregate over the mock rule set. */
export function mockRulesValidation(): RulesValidationDto {
  const rules = mockRuleDtos();
  // Backend semantics: `total` counts rules that LOADED (valid ones only).
  const loaded = rules.filter((r) => r.valid).length;
  return {
    total: loaded,
    errors: 1,
    warnings: 1,
    issues: [
      {
        ruleId: "user/protect-all-of-c",
        message: "match path C:\\ resolves to a drive root — rejected by path-bound validation",
        severity: "error" as const,
      },
      {
        ruleId: "builtin-detection/claude-session-history",
        message:
          "exclude pattern **/.trash/** is shadowed by claude-project-trash-cache at depth 1",
        severity: "warning" as const,
      },
    ],
  };
}
