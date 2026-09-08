# Workspace Integration Tests (placeholder)

This directory is reserved for **workspace-level integration tests** that exercise
more than one crate at a time, mirroring the layout planned in
`doc/spec/DevResidue_SPEC_CN_v0.3.md` §5:

```text
tests/
├── rules/        # Rule Engine end-to-end (Phase 3+)
├── providers/    # Provider -> ScanItem contract (Phase 4+)
├── filesystem/   # Windows filesystem capability tests (Phase 4+)
├── cleanup/      # Planner -> Validator -> Engine flows (Phase 6+)
└── safety/       # Root protection / TOCTOU / reparse invariants (Phase 5+)
```

No test binaries are compiled yet, so do **not** add `*.rs` files here during
Phase 0-2. Cargo only picks up `tests/` directories belonging to an actual
crate target; cross-crate suites will be attached to `devresidue-cli` (the top
consumer crate) or to a dedicated `devresidue-tests` integration package when
the first end-to-end scenario lands.
