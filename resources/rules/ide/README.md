# resources/rules/ide

Rule files in this directory describe **IDE / coding tool data** (editor cache,
indexes, workspace storage, crash reports, ...).

Purpose:

- Classify IDE cache/logs as low risk and IDE workspace storage / local history
  as Review; never treat editor config or credentials as cleanable.
- Provide the knowledge behind the `Ide` residue category.

Rule files are **not implemented yet** — they arrive with the Rule Engine in
Phase 3. Until then this directory intentionally contains only this README.
