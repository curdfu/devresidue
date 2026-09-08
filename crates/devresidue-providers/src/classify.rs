//! Static risk classification for discovered directories (SPEC §13 + §10).
//!
//! Given an artifact-directory *name* (as reported by kondo's `artifact_dirs`
//! or by a dev-cache split), decide whether deleting it costs only a local
//! rebuild (`RegenerableLocal`) or a re-download (`RegenerableDownload`).
//!
//! Anything with an official registry/dependency meaning (dependency trees,
//! vendored packages) is `Download`; everything else is assumed to be
//! rebuildable output. `Review` is used for directories that may hold
//! user-valued state (cargo git checkouts — no registry reference to restore
//! them from).

use devresidue_core::RiskLevel;

/// Directory names whose content is re-fetched from a package registry /
/// lock file rather than rebuilt from sources.
const DOWNLOAD_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "vendor",
    "pods",
    ".terraform",
    "__pypackages__",
    ".pixi",
];

/// The risk of deleting the directory named `name`.
///
/// Matching is case-insensitive; `cmake-build-*` style prefixes are handled
/// by the callers through [`is_local_rebuild_prefix`]. `Review` directories
/// fall through from the caller when they carry special semantics (e.g. cargo
/// git checkouts) — this function returns Local/Download only.
#[must_use]
pub fn dir_risk(name: &str) -> RiskLevel {
    let lower = name.to_lowercase();
    if DOWNLOAD_DIRS.iter().any(|d| *d == lower) {
        RiskLevel::RegenerableDownload
    } else {
        RiskLevel::RegenerableLocal
    }
}

/// True for generated-directory prefixes such as `cmake-build-debug`,
/// `cmake-build-release`, `cmake-build-*`.
#[must_use]
pub fn is_local_rebuild_prefix(name: &str) -> bool {
    name.to_lowercase().starts_with("cmake-build-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_dirs_map_to_download() {
        for d in ["node_modules", ".venv", "vendor", "Pods", ".terraform"] {
            assert_eq!(
                dir_risk(d),
                RiskLevel::RegenerableDownload,
                "{d} must be download-class"
            );
            // Case-insensitive.
            assert_eq!(dir_risk(&d.to_uppercase()), RiskLevel::RegenerableDownload);
        }
    }

    #[test]
    fn build_dirs_map_to_local() {
        for d in [
            "target",
            "build",
            "bin",
            "obj",
            ".gradle",
            "dist-newstyle",
            "_build",
            "Library",
            "Temp",
            "Intermediate",
            "Saved",
            "Binaries",
            "zig-out",
            ".turbo",
            ".nox",
            "__pycache__",
        ] {
            assert_eq!(
                dir_risk(d),
                RiskLevel::RegenerableLocal,
                "{d} must be local-rebuild class"
            );
        }
    }

    #[test]
    fn cmake_build_prefixes_are_local() {
        assert!(is_local_rebuild_prefix("cmake-build-debug"));
        assert!(is_local_rebuild_prefix("CMake-Build-Release"));
        assert!(is_local_rebuild_prefix("cmake-build-whatever"));
        assert!(!is_local_rebuild_prefix("build"));
        assert!(!is_local_rebuild_prefix("cmake-build"));
    }
}
