//! Protected root registry — the roots no cleanup may ever touch (SPEC §18,
//! INV-006).
//!
//! Built from:
//!
//! 1. **every drive root** `X:\` (`A:` … `Z:`);
//! 2. well-known user/system roots resolved from the environment:
//!    `%USERPROFILE%`, `%SYSTEMROOT%`, `%PROGRAMFILES%`,
//!    `%PROGRAMFILES(X86)%`, `%PROGRAMDATA%`;
//! 3. permanently protected credential/config directories under the user
//!    profile: `.ssh`, `.gnupg`, `.aws`, `.azure`, `.kube` (INV-007).
//!
//! The environment may be injected (tests use a fake map); **missing critical
//! environment variables fail construction** (fail-closed — the registry must
//! never silently run with fewer protections than the product promises).
//!
//! Judgement: a target is denied when it *is* a protected root, or when it is
//! an **ancestor** of one — deleting the target would take the protected root
//! down with it. This gives transitive coverage through drive roots
//! (`C:\Users` is denied because `%USERPROFILE%` lives under it) while
//! `C:\Users\alice\AppData\Local\Temp\cache` stays cleanable.

use std::path::{Path, PathBuf};

use super::canonical;

/// The environment keys resolved by [`ProtectedRootRegistry::build`].
pub const REQUIRED_ENV_KEYS: [&str; 5] = [
    "USERPROFILE",
    "SYSTEMROOT",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMDATA",
];

/// Directories permanently protected under `%USERPROFILE%` (INV-007).
const USER_PROFILE_PERMANENT: [&str; 5] = [".ssh", ".gnupg", ".aws", ".azure", ".kube"];

/// Failure mode of construction: a required environment variable is missing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot build the protected-root registry: required environment variable {0} is not set")]
pub struct RegistryInitError(pub String);

/// Why a target matched the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootHitKind {
    /// The target *is* a protected root.
    Equal,
    /// The target is an ancestor of a protected root (deleting it would
    /// remove the protected root as well).
    ContainsProtectedRoot,
}

/// A registry hit describing the protected root involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootHit {
    /// The protected root (normalised).
    pub root: PathBuf,
    pub kind: RootHitKind,
}

/// How environment variables are read. Implemented by a closure or by the
/// process environment.
pub trait EnvSource: Fn(&str) -> Option<String> {}
impl<F> EnvSource for F where F: Fn(&str) -> Option<String> {}

/// The registry of undeletable roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedRootRegistry {
    /// Normalised protected roots.
    roots: Vec<PathBuf>,
}

impl ProtectedRootRegistry {
    /// Builds the registry from a process environment view plus extra roots
    /// the platform layer contributes (e.g. only currently-mounted drives).
    ///
    /// # Errors
    /// Returns [`RegistryInitError`] when any of [`REQUIRED_ENV_KEYS`] is
    /// missing — fail-closed.
    pub fn build(
        env: &dyn EnvSource,
        extra_roots: Vec<PathBuf>,
    ) -> Result<Self, RegistryInitError> {
        let mut roots: Vec<PathBuf> = Vec::new();

        // 1. Every drive root, A:\ .. Z:\. Harmless to include letters that
        //    are not mounted; containment is lexical.
        for letter in 'A'..='Z' {
            roots.push(PathBuf::from(format!("{letter}:\\")));
        }

        // 2. Well-known roots from the environment.
        for key in REQUIRED_ENV_KEYS {
            let Some(value) = env(key) else {
                return Err(RegistryInitError(key.to_string()));
            };
            roots.push(PathBuf::from(value));
        }

        // 3. Permanently protected credential dirs under the user profile.
        let profile = env("USERPROFILE").expect("validated above");
        for sub in USER_PROFILE_PERMANENT {
            roots.push(PathBuf::from(&profile).join(sub));
        }

        roots.extend(extra_roots);

        // Normalise everything once so lookups compare stable forms.
        let roots: Vec<PathBuf> = roots.into_iter().map(canonical::normalize).collect();
        Ok(Self { roots })
    }

    /// R3-G04: merges the scan-persisted workspace roots and the *current*
    /// process's configured workspace roots into the registry's protection
    /// set (workspace roots are never cleanup targets — SPEC §11/§18).
    ///
    /// Merge rule (both sources, union — fail-closed direction):
    ///
    /// - the **scan-persisted** roots (the protection context the scan ran
    ///   with: a target that was a workspace root at scan time keeps its
    ///   protection at plan/execute time even if the config changed);
    /// - the **current** roots (`DEVRESIDUE_WORKSPACE_ROOTS` from the live
    ///   environment / explicit `--projects` of the executing process): a
    ///   workspace configured *after* the scan is honoured before a plan
    ///   built earlier can delete through it.
    ///
    /// The union only ever *adds* protection — removing a workspace root
    /// after the scan cannot un-protect anything the scan protected.
    #[must_use]
    pub fn with_workspace_roots<S, C>(mut self, scan_roots: S, current_roots: C) -> Self
    where
        S: IntoIterator,
        S::Item: AsRef<Path>,
        C: IntoIterator,
        C::Item: AsRef<Path>,
    {
        let mut extra: Vec<PathBuf> = self.roots.clone();
        let mut candidates: Vec<PathBuf> = scan_roots
            .into_iter()
            .map(|r| r.as_ref().to_path_buf())
            .collect();
        candidates.extend(current_roots.into_iter().map(|r| r.as_ref().to_path_buf()));
        for root in candidates {
            let normalised = canonical::normalize(&root);
            if !extra.contains(&normalised) {
                extra.push(normalised);
            }
        }
        self.roots = extra;
        self
    }

    /// Direct construction from an explicit root list (tests / diagnostics).
    /// Skips environment resolution entirely.
    #[must_use]
    pub fn from_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().map(canonical::normalize).collect(),
        }
    }

    /// The protected roots (normalised), for diagnostics/tests.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Reports whether a cleanup of `target` would delete or sit above a
    /// protected root.
    ///
    /// `Some(hit)` means deny. The check is one-directional on purpose:
    /// `is_within(protected_root, target)` is true when the target *is* the
    /// root or an ancestor of it. Deleting an ordinary *descendant* of a
    /// protected root (a cache inside the user profile) is what cleanup does
    /// every day and is allowed here.
    ///
    /// A target that **is itself a UNC share root** (`\\server\share`,
    /// `\\?\UNC\server\share`) is denied lexically even when the share was not
    /// enumerated at registry construction (F13): the share root is the
    /// natural undeletable boundary — deleting it would detach the whole
    /// share, and we never know what lives above it.
    #[must_use]
    pub fn protection_hit(&self, target: &Path) -> Option<RootHit> {
        if let Some(root) = canonical::unc_share_root(target) {
            return Some(RootHit {
                root,
                kind: RootHitKind::Equal,
            });
        }
        for root in &self.roots {
            if canonical::is_within(root, target) {
                let kind = if canonical::is_within(target, root) {
                    RootHitKind::Equal
                } else {
                    RootHitKind::ContainsProtectedRoot
                };
                return Some(RootHit {
                    root: root.clone(),
                    kind,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A realistic fake environment.
    fn fake_env() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("USERPROFILE".into(), r"C:\Users\alice".into());
        m.insert("SYSTEMROOT".into(), r"C:\Windows".into());
        m.insert("PROGRAMFILES".into(), r"C:\Program Files".into());
        m.insert("PROGRAMFILES(X86)".into(), r"C:\Program Files (x86)".into());
        m.insert("PROGRAMDATA".into(), r"C:\ProgramData".into());
        m
    }

    fn registry(env: &HashMap<String, String>) -> ProtectedRootRegistry {
        ProtectedRootRegistry::build(&|k: &str| env.get(k).cloned(), vec![]).expect("fake env ok")
    }

    #[test]
    fn missing_critical_env_fails_closed() {
        let mut env = fake_env();
        env.remove("USERPROFILE");
        let err = ProtectedRootRegistry::build(&|k: &str| env.get(k).cloned(), vec![]);
        assert!(matches!(err, Err(RegistryInitError(key)) if key == "USERPROFILE"));

        let mut env = fake_env();
        env.remove("PROGRAMDATA");
        assert!(ProtectedRootRegistry::build(&|k: &str| env.get(k).cloned(), vec![]).is_err());
    }

    #[test]
    fn drive_roots_and_exact_matches_are_denied() {
        let reg = registry(&fake_env());
        assert_eq!(
            reg.protection_hit(Path::new("C:\\")).unwrap().kind,
            RootHitKind::Equal
        );
        assert_eq!(
            reg.protection_hit(Path::new("D:\\")).unwrap().kind,
            RootHitKind::Equal
        );
        assert!(reg.protection_hit(Path::new("C:\\Windows")).is_some());
    }

    #[test]
    fn ancestor_of_protected_root_is_denied_transitively() {
        let reg = registry(&fake_env());
        // Deleting C:\Users would delete %USERPROFILE%.
        assert_eq!(
            reg.protection_hit(Path::new("C:\\Users")).unwrap().kind,
            RootHitKind::ContainsProtectedRoot
        );
        // Deleting C:\Program Files (x86) -> protected root itself.
        assert!(reg
            .protection_hit(Path::new("C:\\Program Files (x86)"))
            .is_some());
        // Even case-/separator-variants hit.
        assert!(reg.protection_hit(Path::new("c:/users")).is_some());
    }

    #[test]
    fn permanent_credential_dirs_are_denied() {
        let reg = registry(&fake_env());
        for sub in [".ssh", ".gnupg", ".aws", ".azure", ".kube"] {
            let target = format!(r"C:\Users\alice\{sub}");
            assert_eq!(
                reg.protection_hit(Path::new(&target)).unwrap().kind,
                RootHitKind::Equal,
                "{sub} must be protected"
            );
        }
    }

    #[test]
    fn sibling_prefix_is_not_denied() {
        let reg = registry(&fake_env());
        // USERPROFILE = C:\Users\alice. C:\foo-bar shares no component.
        assert!(reg.protection_hit(Path::new("C:\\foo-bar")).is_none());
        assert!(reg.protection_hit(Path::new("C:\\Users")).is_some());
        // C:\User (singular) is not an ancestor of C:\Users\alice.
        assert!(reg.protection_hit(Path::new("C:\\User")).is_none());
    }

    #[test]
    fn ordinary_descendants_of_protected_roots_are_not_denied() {
        let reg = registry(&fake_env());
        // A cache inside the user profile is cleanable at the registry level.
        assert!(reg
            .protection_hit(Path::new(
                "C:\\Users\\alice\\AppData\\Local\\Temp\\devresidue-cache"
            ))
            .is_none());
        assert!(reg
            .protection_hit(Path::new("C:\\ProgramData\\whatever"))
            .is_none());
    }

    #[test]
    fn extra_roots_from_platform_are_honoured() {
        let env = fake_env();
        let reg = ProtectedRootRegistry::build(
            &|k: &str| env.get(k).cloned(),
            vec![PathBuf::from(r"\\server\share")],
        )
        .unwrap();
        assert!(reg.protection_hit(Path::new("\\\\server\\share")).is_some());
        // Child of the UNC root is a descendant — allowed.
        assert!(reg
            .protection_hit(Path::new("\\\\server\\share\\build-cache"))
            .is_none());
        // Deleting \\server\share\a would take \\server\share (protected
        // only when the share itself is the root of an ancestor dir) with it
        // only if the share sat *under* it — which a share root does not.
        // A sub-share scenario is covered by the drive-root transitive case
        // above; assert the lexical containment instead.
        assert!(!canonical::is_within("\\\\server", "\\\\server\\share"));
    }

    // ---- R3-G04: workspace roots join the protection set ----------------

    #[test]
    fn r3g04_scan_persisted_workspace_roots_are_protected() {
        // The scan ran with the workspace as a protection root; the registry
        // must deny that root (and any ancestor of it) even when the current
        // env has nothing configured.
        let workspace = r"D:\review\workspace";
        let reg =
            registry(&fake_env()).with_workspace_roots([workspace], std::iter::empty::<&Path>());
        // Equal hit: the workspace root itself is never a cleanup target.
        assert_eq!(
            reg.protection_hit(Path::new(workspace)).unwrap().kind,
            RootHitKind::Equal
        );
        // Ancestor: deleting D:\review would take the workspace with it.
        assert!(matches!(
            reg.protection_hit(Path::new(r"D:\review")).unwrap().kind,
            RootHitKind::ContainsProtectedRoot
        ));
        // A descendant cache inside the workspace stays cleanable (only the
        // root itself is protected).
        assert!(reg
            .protection_hit(Path::new(r"D:\review\workspace\target"))
            .is_none());
    }

    #[test]
    fn r3g04_current_env_workspace_roots_configured_after_the_scan_are_protected() {
        // The exact G04 scenario: X was a cleanable cache root at scan time;
        // the user configured X as a workspace root in the *executing*
        // process. The union must deny X now.
        let cache = r"E:\projects\my-cache";
        let reg = registry(&fake_env()).with_workspace_roots(std::iter::empty::<&Path>(), [cache]);
        assert_eq!(
            reg.protection_hit(Path::new(cache)).unwrap().kind,
            RootHitKind::Equal
        );
    }

    #[test]
    fn r3g04_union_only_adds_protection_and_deduplicates() {
        let ws = r"D:\ws";
        let other = r"E:\other\ws";
        let reg = registry(&fake_env()).with_workspace_roots([ws, ws], [ws, other]);
        assert_eq!(
            reg.protection_hit(Path::new(ws)).unwrap().kind,
            RootHitKind::Equal
        );
        assert!(reg.protection_hit(Path::new(other)).is_some());
        // A scan-persisted root keeps its protection even when the current
        // env no longer lists it (the union only ever adds).
        let kept = r"D:\kept";
        let reg2 = registry(&fake_env()).with_workspace_roots([kept], std::iter::empty::<&Path>());
        assert!(reg2.protection_hit(Path::new(kept)).is_some());
    }

    #[test]
    fn unc_share_roots_are_denied_without_registration() {
        // F13: a UNC share root is denied purely by its shape — no share
        // enumeration needed, plain or extended spelling.
        let reg = registry(&fake_env()); // no UNC roots registered at all
        let hit = reg.protection_hit(Path::new(r"\\srv\share")).unwrap();
        assert_eq!(hit.kind, RootHitKind::Equal);
        let verbatim = reg.protection_hit(Path::new(r"\\?\UNC\srv\share")).unwrap();
        assert_eq!(verbatim.kind, RootHitKind::Equal);

        // A path *below* a share root is not itself a root and stays allowed.
        assert!(reg
            .protection_hit(Path::new(r"\\srv\share\build-cache"))
            .is_none());
        // Bare server with no share is not a share root (nothing to delete at
        // that lexical level) — but a share directly under it would still be
        // caught when targeted.
        assert!(reg.protection_hit(Path::new(r"\\srv")).is_none());
    }
}
