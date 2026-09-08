# resources/rules/dev_cache

Rule files in this directory describe **developer tool cache locations**
(npm, pnpm, yarn, bun, pip, uv, cargo, rustup, Go, Gradle, Maven, NuGet, ...).

Purpose:

- Location knowledge used together with the Mole-derived cache paths
  (`tw93/Mole` Windows branch, MIT) to discover cache roots.
- Caches are generally `RegenerableDownload`: deleting means a later
  re-download. Cleanup must prefer **tool-native commands** (ExternalCommand)
  or validated cleanup through the CleanupEngine only (SPEC §10, INV-011).

Rule files are **not implemented yet** — they arrive with the Rule Engine in
Phase 3. Until then this directory intentionally contains only this README.
