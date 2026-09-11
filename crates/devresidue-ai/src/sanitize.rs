//! Metadata-only, fail-closed preparation for an AI review batch.
//!
//! The sanitizer consumes only fields already present on a trusted
//! `ScanItem`. The path is used as an in-memory lexical input for zone/depth
//! classification and for detecting path-derived fragments in human labels;
//! it is never copied into a serialized output type. Evidence details,
//! explanations, cleanup actions, snapshots, and classification-rule data are
//! intentionally not read.

use std::path::Path;
use std::time::{Duration, SystemTime};

use devresidue_core::ai::{
    AgeBucket, AiBatchId, AiEntryToken, AiProfileId, AiZone, PreparedBatch, SanitizedEntry,
    SizeBucket,
};
use devresidue_core::{Evidence, ResidueCategory, RiskLevel, ScanItem, SourceKind};
use uuid::Uuid;

/// Hard privacy and request-size cap for one prepared batch.
pub const MAX_BATCH_ITEMS: usize = 20;

/// Maximum number of Unicode scalar values retained in a non-sensitive hint.
pub const MAX_HINT_CHARS: usize = 64;

const SENSITIVE_METADATA: &str = "<sensitive-metadata>";
const UNKNOWN_DISPLAY_NAME: &str = "<unknown>";
const UNDER_ONE_DAY: Duration = Duration::from_secs(86_400);
const UNDER_SEVEN_DAYS: Duration = Duration::from_secs(7 * 86_400);
const UNDER_THIRTY_DAYS: Duration = Duration::from_secs(30 * 86_400);
const UNDER_ONE_HUNDRED_EIGHTY_DAYS: Duration = Duration::from_secs(180 * 86_400);
const ONE_MIB: u64 = 1_048_576;
const SIXTY_FOUR_MIB: u64 = 64 * ONE_MIB;
const ONE_GIB: u64 = 1_073_741_824;

/// Creates sanitized, metadata-only batches without reading the filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataSanitizer;

impl MetadataSanitizer {
    /// Creates a stateless sanitizer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Prepares at most [`MAX_BATCH_ITEMS`] `Unknown`/`Review` items.
    ///
    /// The caller owns snapshot integrity validation and supplies only the
    /// registered item slice plus its scan generation. This method does not
    /// accept a directory walker, reader, snapshot store, or arbitrary path.
    pub fn prepare(
        &self,
        items: &[ScanItem],
        scan_generation: u64,
        profile_id: AiProfileId,
    ) -> Result<PreparedBatch, String> {
        let now = SystemTime::now();
        let id = AiBatchId::new(random_uuid());
        let mut entries = Vec::with_capacity(MAX_BATCH_ITEMS.min(items.len()));

        for item in items {
            if !is_ai_candidate(item) {
                continue;
            }
            if entries.len() == MAX_BATCH_ITEMS {
                break;
            }

            let path_context = PathContext::from_path(&item.path);
            let (zone, relative_depth) = zone_and_depth(&item.path);
            let display_name = sanitize_hint(&item.display_name, &path_context)
                .unwrap_or_else(|| UNKNOWN_DISPLAY_NAME.to_owned());
            let product_hint = item
                .product
                .as_deref()
                .and_then(|product| sanitize_hint(product, &path_context));

            entries.push(SanitizedEntry {
                id: AiEntryToken::new(random_uuid()),
                zone,
                relative_depth,
                display_name,
                source_kind: source_label(item.source).to_owned(),
                category_hint: category_label(item.category).to_owned(),
                product_hint,
                size_bucket: size_bucket(item.logical_size),
                age_bucket: age_bucket(item.last_modified, now),
                signals: structural_signals(&item.evidence),
            });
        }

        Ok(PreparedBatch {
            id,
            profile_id,
            scan_generation,
            entries,
        })
    }
}

fn random_uuid() -> String {
    Uuid::new_v4().to_string()
}

fn is_ai_candidate(item: &ScanItem) -> bool {
    // Cleanup intent stays local and is never serialized. It must not prevent
    // an explicitly selected Unknown/Review item from receiving an AI suggestion.
    matches!(item.risk, RiskLevel::Unknown | RiskLevel::Review)
}

fn size_bucket(size: u64) -> SizeBucket {
    if size < ONE_MIB {
        SizeBucket::Under1MiB
    } else if size < SIXTY_FOUR_MIB {
        SizeBucket::Under64MiB
    } else if size < ONE_GIB {
        SizeBucket::Under1GiB
    } else {
        SizeBucket::AtLeast1GiB
    }
}

fn age_bucket(last_modified: Option<SystemTime>, now: SystemTime) -> AgeBucket {
    let Some(last_modified) = last_modified else {
        return AgeBucket::Unknown;
    };

    // A future timestamp can result from clock skew. Treat it as the safest
    // coarse age rather than manufacturing a negative duration or rejecting
    // the whole batch.
    let age = match now.duration_since(last_modified) {
        Ok(age) => age,
        Err(_) => return AgeBucket::Under1Day,
    };

    if age < UNDER_ONE_DAY {
        AgeBucket::Under1Day
    } else if age < UNDER_SEVEN_DAYS {
        AgeBucket::Under7Days
    } else if age < UNDER_THIRTY_DAYS {
        AgeBucket::Under30Days
    } else if age < UNDER_ONE_HUNDRED_EIGHTY_DAYS {
        AgeBucket::Under180Days
    } else {
        AgeBucket::AtLeast180Days
    }
}

fn source_label(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Rule => "rule",
        SourceKind::Kondo => "kondo",
        SourceKind::DeveloperCacheProvider => "developer-cache-provider",
        SourceKind::PackageManager => "package-manager",
        SourceKind::AgentProvider => "agent-provider",
        SourceKind::UnknownProvider => "unknown-provider",
    }
}

fn category_label(category: ResidueCategory) -> &'static str {
    match category {
        ResidueCategory::AiAgent => "ai-agent",
        ResidueCategory::Ide => "ide",
        ResidueCategory::DeveloperCache => "developer-cache",
        ResidueCategory::PackageCache => "package-cache",
        ResidueCategory::BuildArtifact => "build-artifact",
        ResidueCategory::Dependency => "dependency",
        ResidueCategory::Log => "log",
        ResidueCategory::Temporary => "temporary",
        // These category names are themselves sensitive metadata labels. Keep
        // the category hint useful only as a fixed generic class so the
        // forbidden credential/session vocabulary cannot re-enter the payload.
        ResidueCategory::Session | ResidueCategory::Credential => SENSITIVE_METADATA,
        ResidueCategory::WorkspaceState => "workspace-state",
        ResidueCategory::Configuration => "configuration",
        ResidueCategory::Unknown => "unknown",
    }
}

fn structural_signals(evidence: &[Evidence]) -> Vec<String> {
    let mut signals = Vec::new();
    for entry in evidence {
        let Some(signal) = evidence_signal(&entry.source) else {
            continue;
        };
        if !signals.iter().any(|existing| existing == signal) {
            signals.push(signal.to_owned());
        }
    }
    signals
}

fn evidence_signal(source: &str) -> Option<&'static str> {
    // Match the source key exactly and emit a fixed label. In particular, the
    // evidence detail is never inspected or copied.
    match source {
        "path-layout" => Some("path-layout"),
        "tool-reported-path" => Some("tool-reported-path"),
        "kondo-project-type" => Some("project-type"),
        "manifest" => Some("manifest-marker"),
        "category-clue" => Some("category-clue"),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct PathContext {
    path_fragments: Vec<String>,
    identity_fragments: Vec<String>,
}

impl PathContext {
    fn from_path(path: &Path) -> Self {
        let segments = lexical_segments(path);
        let mut context = Self::default();

        if segments.len() > 1 {
            context.path_fragments = segments[..segments.len() - 1]
                .iter()
                .filter(|segment| !is_generic_path_segment(segment))
                .cloned()
                .collect();

            let final_index = segments.len() - 1;
            if is_user_root_boundary(&segments[final_index - 1]) {
                context
                    .identity_fragments
                    .push(segments[final_index].clone());
            }
        }

        context
    }
}

fn sanitize_hint(text: &str, context: &PathContext) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }

    let lower = text.to_ascii_lowercase();
    if contains_forbidden_metadata(&lower, text, context) {
        return Some(SENSITIVE_METADATA.to_owned());
    }

    Some(truncate_chars(text, MAX_HINT_CHARS))
}

fn contains_forbidden_metadata(lower: &str, original: &str, context: &PathContext) -> bool {
    if !original.is_ascii()
        || original.chars().any(char::is_control)
        || original.contains('/')
        || original.contains('\\')
        || has_drive_prefix(original)
        || lower.contains("://")
    {
        return true;
    }

    const SENSITIVE_TERMS: &[&str] = &[
        "credential",
        "credentials",
        "auth",
        "token",
        "tokens",
        "secret",
        "secrets",
        "password",
        "passwords",
        "passwd",
        "login",
        "logins",
        "cookie",
        "cookies",
        "session",
        "sessions",
        "api",
        "apis",
        "apikey",
        "key",
        "keys",
        "host",
        "hostname",
        "volume",
        "users",
        "userprofile",
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        ".env",
        ".htpasswd",
        "netrc",
        "credentials.json",
        ".ssh",
        "ssh",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "pem",
        "p12",
        "pfx",
        "env",
        "htpasswd",
    ];
    let tokens = lexical_tokens(lower);
    if SENSITIVE_TERMS
        .iter()
        .any(|term| contains_token_sequence(&tokens, &lexical_tokens(term)))
    {
        return true;
    }

    const CREDENTIAL_MANAGER_PRODUCTS: &[&str] = &["1password", "lastpass", "bitwarden", "keepass"];
    if CREDENTIAL_MANAGER_PRODUCTS
        .iter()
        .any(|term| contains_token_sequence(&tokens, &lexical_tokens(term)))
    {
        return true;
    }

    const SENSITIVE_PREFIX_MARKERS: &[&str] = &["token", "credential", "secret"];
    if tokens.iter().any(|token| {
        SENSITIVE_PREFIX_MARKERS
            .iter()
            .any(|prefix| token.starts_with(prefix))
    }) {
        return true;
    }

    const SECRET_MARKERS: &[&str] = &[
        "bearer ",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "akia",
        "-----begin",
        "eyj",
    ];
    if SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return true;
    }

    context
        .path_fragments
        .iter()
        .chain(context.identity_fragments.iter())
        .any(|fragment| {
            let fragment_tokens = lexical_tokens(fragment);
            !fragment.is_empty()
                && (lower.contains(fragment) || contains_token_sequence(&tokens, &fragment_tokens))
        })
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut chars = text.chars();
    let result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let keep = max_chars.saturating_sub(1);
        let truncated: String = result.chars().take(keep).collect();
        format!("{truncated}…")
    } else {
        result
    }
}

fn has_drive_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_drive_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_generic_path_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == "." || segment == ".." || is_drive_segment(segment) {
        return true;
    }

    const GENERIC_SEGMENTS: &[&str] = &[
        "users",
        "user",
        "home",
        "homes",
        "profile",
        "profiles",
        "userprofile",
        "appdata",
        "roaming",
        "local",
        "workspace",
        "workspaces",
        "project",
        "projects",
        "repo",
        "repos",
        "repository",
        "repositories",
        "cache",
        "caches",
        "tmp",
        "temp",
        "temporary",
        "var",
        "srv",
        "data",
        "root",
        "public",
        "private",
        "documents",
        "downloads",
        "desktop",
        "programdata",
        "program",
        "files",
        "library",
        "application",
        "support",
        "target",
        "build",
        "bin",
        "obj",
        "node_modules",
    ];

    GENERIC_SEGMENTS.contains(&segment)
}

fn is_user_root_boundary(segment: &str) -> bool {
    matches!(
        segment,
        "users" | "home" | "profile" | "profiles" | "userprofile"
    )
}

fn lexical_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn contains_token_sequence(tokens: &[String], needle: &[String]) -> bool {
    if needle.is_empty() || needle.len() > tokens.len() {
        return false;
    }

    tokens.windows(needle.len()).any(|window| window == needle)
}

fn lexical_segments(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::to_ascii_lowercase)
        .collect()
}

fn find_segment(segments: &[String], wanted: &str) -> Option<usize> {
    segments.iter().position(|segment| segment == wanted)
}

fn zone_and_depth(path: &Path) -> (AiZone, u8) {
    let segments = lexical_segments(path);

    if let Some(index) = find_app_data_boundary(&segments, "roaming") {
        return (AiZone::AppData, depth_after(&segments, index));
    }
    if let Some(index) = find_app_data_boundary(&segments, "local") {
        return (AiZone::LocalAppData, depth_after(&segments, index));
    }
    if let Some(index) = find_workspace_boundary(&segments) {
        return (AiZone::Workspace, depth_after(&segments, index));
    }
    if let Some(index) = find_segment(&segments, "users") {
        return (
            AiZone::Home,
            depth_after(&segments, index.saturating_add(2)),
        );
    }
    if let Some(index) = find_segment(&segments, "home") {
        return (
            AiZone::Home,
            depth_after(&segments, index.saturating_add(2)),
        );
    }

    (AiZone::Other, depth_after(&segments, 0))
}

fn find_app_data_boundary(segments: &[String], leaf: &str) -> Option<usize> {
    segments
        .windows(2)
        .position(|window| window[0] == "appdata" && window[1] == leaf)
        .map(|index| index + 2)
}

fn find_workspace_boundary(segments: &[String]) -> Option<usize> {
    const WORKSPACE_MARKERS: &[&str] = &[
        "workspace",
        "workspaces",
        "project",
        "projects",
        "repo",
        "repositories",
    ];
    segments
        .iter()
        .position(|segment| WORKSPACE_MARKERS.contains(&segment.as_str()))
        .map(|index| index + 1)
}

fn depth_after(segments: &[String], boundary: usize) -> u8 {
    segments.len().saturating_sub(boundary).min(8) as u8
}
