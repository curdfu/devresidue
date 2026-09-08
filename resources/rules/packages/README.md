# resources/rules/packages

Rule files in this directory describe **project dependency directories**
(`node_modules`, `.venv`, package-manager stores inside projects, ...).

Purpose:

- Identify dependency trees as `RegenerableDownload` (they must be re-fetched).
- Dependencies inside a project are typically handled by the tool itself
  (`npm ci`, `uv sync`, ...); rules here only classify and explain.

Rule files are **not implemented yet** — they arrive with the Rule Engine in
Phase 3. Until then this directory intentionally contains only this README.
