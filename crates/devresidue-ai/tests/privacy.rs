use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use devresidue_ai::MetadataSanitizer;
use devresidue_core::ai::{AgeBucket, AiProfileId, AiZone, SizeBucket};
use devresidue_core::{
    CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, ScanItemId, SourceKind,
};

fn profile_id() -> AiProfileId {
    AiProfileId::parse("018f7e21-9d15-7b17-a5fd-4f0f2bcadc72").unwrap()
}

fn item(id: u64, risk: RiskLevel, path: &str) -> ScanItem {
    ScanItem {
        id: ScanItemId::from_raw(id),
        path: PathBuf::from(path),
        display_name: format!("safe display {id}"),
        product: Some("safe product".into()),
        category: ResidueCategory::DeveloperCache,
        risk,
        source: SourceKind::DeveloperCacheProvider,
        logical_size: 123,
        file_count: 4,
        last_modified: Some(SystemTime::now() - Duration::from_secs(60)),
        explanation: "must never be serialized by the sanitizer".into(),
        cleanup_action: CleanupAction::None,
        evidence: vec![Evidence::new("path-layout", "must never be serialized")],
        scan_snapshot: None,
        classification_rule_id: None,
    }
}

#[test]
fn sanitizer_excludes_protected_safe_and_regenerable_items() {
    let items = vec![
        item(1, RiskLevel::Protected, r"C:\Users\alice\.aws\credentials"),
        item(2, RiskLevel::Safe, r"C:\Users\alice\.cache\safe"),
        item(3, RiskLevel::RegenerableLocal, r"C:\Users\alice\target"),
        item(
            4,
            RiskLevel::RegenerableDownload,
            r"C:\Users\alice\node_modules",
        ),
        item(5, RiskLevel::Unknown, r"C:\Users\alice\unknown"),
        item(6, RiskLevel::Review, r"C:\Users\alice\review"),
    ];

    let batch = MetadataSanitizer::new()
        .prepare(&items, 7, profile_id())
        .unwrap();
    assert_eq!(batch.entries.len(), 2);
}

#[test]
fn sanitizer_allows_only_unknown_and_review_and_caps_batch_at_twenty() {
    let items: Vec<_> = (0..30)
        .map(|id| item(id, RiskLevel::Unknown, &format!(r"D:\workspace\item-{id}")))
        .collect();

    let batch = MetadataSanitizer::new()
        .prepare(&items, 7, profile_id())
        .unwrap();
    assert_eq!(batch.entries.len(), 20);
    assert!(batch.entries.iter().all(|entry| entry.relative_depth <= 8));
}

#[test]
fn sanitizer_keeps_unknown_and_review_items_regardless_of_local_cleanup_intent() {
    let valid = item(20, RiskLevel::Unknown, r"D:\workspace\valid");
    let mut has_cleanup = item(21, RiskLevel::Unknown, r"D:\workspace\cleanup");
    has_cleanup.cleanup_action = CleanupAction::RecycleBin;
    let mut locally_classified_review = item(22, RiskLevel::Review, r"D:\workspace\classified");
    locally_classified_review.classification_rule_id = Some("builtin-detection/session".into());

    let batch = MetadataSanitizer::new()
        .prepare(
            &[valid, has_cleanup, locally_classified_review],
            7,
            profile_id(),
        )
        .unwrap();

    assert_eq!(batch.entries.len(), 3);
}

#[test]
fn sensitive_paths_names_products_and_evidence_details_never_enter_json() {
    let mut sensitive = item(
        42,
        RiskLevel::Review,
        r"\\corp-host\volume\Users\alice\AppData\Roaming\tool\token-store",
    );
    sensitive.display_name = r"C:\Users\alice\.ssh\id_rsa".into();
    sensitive.product = Some("Acme auth API key sk-test-never-send".into());
    sensitive.explanation = "credential password login cookies session".into();
    sensitive.evidence = vec![Evidence::new(
        "manifest",
        "raw evidence detail with C:\\Users\\alice and Bearer sk-secret",
    )];

    let batch = MetadataSanitizer::new()
        .prepare(&[sensitive], 8, profile_id())
        .unwrap();
    let payload = serde_json::to_string(&batch).unwrap();

    for forbidden in [
        "corp-host",
        "volume",
        "Users",
        "alice",
        "AppData",
        "token",
        "ssh",
        "id_rsa",
        "credentials",
        "credential",
        "password",
        "login",
        "cookies",
        "session",
        "auth",
        "API",
        "sk-test",
        "Bearer",
        "raw evidence detail",
    ] {
        assert!(
            !payload.contains(forbidden),
            "payload leaked {forbidden}: {payload}"
        );
    }
    assert!(payload.contains("<sensitive-metadata>"));
}

#[test]
fn custom_profile_usernames_are_redacted_from_windows_and_unix_hints() {
    let mut windows = item(43, RiskLevel::Review, r"C:\Profiles\alice\cache");
    windows.display_name = "alice cache".into();
    windows.product = Some("alice toolkit".into());

    let mut unix = item(44, RiskLevel::Unknown, "/srv/home/bob/cache");
    unix.display_name = "bob cache".into();
    unix.product = Some("bob toolkit".into());

    let batch = MetadataSanitizer::new()
        .prepare(&[windows, unix], 8, profile_id())
        .unwrap();
    let payload = serde_json::to_string(&batch).unwrap();

    assert!(!payload.contains("alice"), "payload leaked alice: {payload}");
    assert!(!payload.contains("bob"), "payload leaked bob: {payload}");
    assert_eq!(batch.entries[0].display_name, "<sensitive-metadata>");
    assert_eq!(batch.entries[0].product_hint.as_deref(), Some("<sensitive-metadata>"));
    assert_eq!(batch.entries[1].display_name, "<sensitive-metadata>");
    assert_eq!(batch.entries[1].product_hint.as_deref(), Some("<sensitive-metadata>"));
}

#[test]
fn user_root_last_leaf_is_redacted_but_ordinary_workspace_leaf_is_not() {
    let cases = [
        (60, r"C:\Users\alice", "alice"),
        (61, r"C:\Profiles\alice", "alice"),
        (62, "/srv/home/bob", "bob"),
    ];

    for (id, path, username) in cases {
        let mut candidate = item(id, RiskLevel::Review, path);
        candidate.display_name = format!("{username} profile");
        candidate.product = Some(format!("{username} toolkit"));

        let batch = MetadataSanitizer::new()
            .prepare(&[candidate], 15, profile_id())
            .unwrap();
        let entry = &batch.entries[0];
        assert_eq!(entry.display_name, "<sensitive-metadata>");
        assert_eq!(entry.product_hint.as_deref(), Some("<sensitive-metadata>"));
    }

    let mut ordinary = item(63, RiskLevel::Unknown, r"D:\workspace\keyboard");
    ordinary.display_name = "keyboard".into();
    ordinary.product = Some("keyboard".into());
    let batch = MetadataSanitizer::new()
        .prepare(&[ordinary], 15, profile_id())
        .unwrap();
    assert_eq!(batch.entries[0].display_name, "keyboard");
    assert_eq!(batch.entries[0].product_hint.as_deref(), Some("keyboard"));
}

#[test]
fn path_fragments_are_matched_after_separator_normalization() {
    let mut candidate = item(64, RiskLevel::Review, r"C:\Profiles\alice_smith\cache");
    candidate.display_name = "alice smith cache".into();
    candidate.product = Some("alice-smith toolkit".into());

    let batch = MetadataSanitizer::new()
        .prepare(&[candidate], 15, profile_id())
        .unwrap();
    let entry = &batch.entries[0];
    assert_eq!(entry.display_name, "<sensitive-metadata>");
    assert_eq!(entry.product_hint.as_deref(), Some("<sensitive-metadata>"));
}

#[test]
fn credential_manager_products_and_sensitive_prefix_markers_are_redacted() {
    for (id, hint) in [
        (65, "1Password"),
        (66, "LastPass"),
        (67, "Bitwarden"),
        (68, "KeePass"),
        (69, "token123"),
        (70, "credentials_backup"),
        (71, "secretStore"),
    ] {
        let mut candidate = item(id, RiskLevel::Review, r"D:\workspace\ordinary");
        candidate.display_name = hint.into();
        candidate.product = Some(hint.into());

        let batch = MetadataSanitizer::new()
            .prepare(&[candidate], 15, profile_id())
            .unwrap();
        let entry = &batch.entries[0];
        assert_eq!(entry.display_name, "<sensitive-metadata>", "hint: {hint}");
        assert_eq!(
            entry.product_hint.as_deref(),
            Some("<sensitive-metadata>"),
            "product: {hint}"
        );
    }

    let mut ordinary = item(72, RiskLevel::Unknown, r"D:\workspace\ordinary");
    ordinary.display_name = "keyboard".into();
    ordinary.product = Some("keyboard".into());
    let batch = MetadataSanitizer::new()
        .prepare(&[ordinary], 15, profile_id())
        .unwrap();
    assert_eq!(batch.entries[0].display_name, "keyboard");
    assert_eq!(batch.entries[0].product_hint.as_deref(), Some("keyboard"));
}

#[test]
fn non_ascii_and_zero_width_hints_are_replaced() {
    for (id, hint) in [(73, "工具缓存"), (74, "to\u{200B}ken")] {
        let mut candidate = item(id, RiskLevel::Unknown, r"D:\workspace\ordinary");
        candidate.display_name = hint.into();
        candidate.product = Some(hint.into());

        let batch = MetadataSanitizer::new()
            .prepare(&[candidate], 15, profile_id())
            .unwrap();
        let entry = &batch.entries[0];
        assert_eq!(entry.display_name, "<sensitive-metadata>", "hint: {hint}");
        assert_eq!(
            entry.product_hint.as_deref(),
            Some("<sensitive-metadata>"),
            "product: {hint}"
        );
        let payload = serde_json::to_string(entry).unwrap();
        assert!(!payload.contains(hint), "payload retained Unicode hint: {payload}");
    }
}

#[test]
fn non_generic_registered_path_fragments_are_redacted_from_hints() {
    let mut candidate = item(45, RiskLevel::Review, r"D:\workspaces\project-alpha\cache");
    candidate.display_name = "project-alpha cache".into();
    candidate.product = Some("cache for project-alpha".into());

    let batch = MetadataSanitizer::new()
        .prepare(&[candidate], 8, profile_id())
        .unwrap();
    let entry = &batch.entries[0];

    assert_eq!(entry.display_name, "<sensitive-metadata>");
    assert_eq!(entry.product_hint.as_deref(), Some("<sensitive-metadata>"));
    let payload = serde_json::to_string(entry).unwrap();
    assert!(!payload.contains("project-alpha"));
}

#[test]
fn path_uri_control_and_sensitive_file_hints_are_replaced_wholesale() {
    for (id, hint) in [
        (46, "C:\\Users\\alice\\cache"),
        (47, "https://example.invalid/.env"),
        (48, "certificate.pem"),
        (49, "keystore.p12"),
        (50, "settings.env"),
        (51, "control\u{0007}character"),
    ] {
        let mut candidate = item(id, RiskLevel::Unknown, r"D:\workspace\ordinary");
        candidate.display_name = hint.into();

        let batch = MetadataSanitizer::new()
            .prepare(&[candidate], 8, profile_id())
            .unwrap();
        assert_eq!(
            batch.entries[0].display_name,
            "<sensitive-metadata>",
            "hint should be replaced: {hint}"
        );
        let payload = serde_json::to_string(&batch.entries[0]).unwrap();
        assert!(!payload.contains(hint), "payload retained hint {hint}: {payload}");
    }
}

#[test]
fn sensitive_terms_use_boundaries_without_replacing_keyboard() {
    let mut keyboard = item(52, RiskLevel::Unknown, r"D:\workspace\keyboard");
    keyboard.display_name = "keyboard".into();
    keyboard.product = Some("keyboard".into());

    let safe_batch = MetadataSanitizer::new()
        .prepare(&[keyboard], 8, profile_id())
        .unwrap();
    assert_eq!(safe_batch.entries[0].display_name, "keyboard");
    assert_eq!(safe_batch.entries[0].product_hint.as_deref(), Some("keyboard"));

    for (id, hint) in [
        (53, "api_key"),
        (54, "api-key"),
        (55, "secret token"),
    ] {
        let mut candidate = item(id, RiskLevel::Review, r"D:\workspace\ordinary");
        candidate.display_name = hint.into();
        let batch = MetadataSanitizer::new()
            .prepare(&[candidate], 8, profile_id())
            .unwrap();
        assert_eq!(batch.entries[0].display_name, "<sensitive-metadata>");
    }
}

#[test]
fn evidence_allowlist_emits_only_fixed_signals_and_never_detail() {
    let mut candidate = item(56, RiskLevel::Unknown, r"D:\workspace\evidence");
    candidate.evidence = vec![
        Evidence::new("path-layout", "path-layout detail must not escape"),
        Evidence::new("tool-reported-path", "tool path detail must not escape"),
        Evidence::new("kondo-project-type", "kondo detail must not escape"),
        Evidence::new("manifest", "manifest detail must not escape"),
        Evidence::new("category-clue", "category detail must not escape"),
        Evidence::new("not-allowed", "unknown detail must not escape"),
    ];

    let batch = MetadataSanitizer::new()
        .prepare(&[candidate], 8, profile_id())
        .unwrap();
    let payload = serde_json::to_string(&batch.entries[0]).unwrap();

    assert_eq!(
        batch.entries[0].signals,
        vec![
            "path-layout",
            "tool-reported-path",
            "project-type",
            "manifest-marker",
            "category-clue",
        ]
    );
    for detail in [
        "path-layout detail must not escape",
        "tool path detail must not escape",
        "kondo detail must not escape",
        "manifest detail must not escape",
        "category detail must not escape",
        "unknown detail must not escape",
    ] {
        assert!(!payload.contains(detail), "payload leaked evidence detail: {payload}");
    }
}

#[test]
fn serialized_entry_is_exactly_the_task_one_whitelist() {
    let mut candidate = item(8, RiskLevel::Unknown, r"D:\workspace\cache");
    candidate.display_name = "ordinary-cache".into();
    candidate.product = Some("ordinary product".into());
    candidate.evidence = vec![
        Evidence::new("path-layout", "not serialized"),
        Evidence::new("unknown-source", "not serialized"),
    ];
    let batch = MetadataSanitizer::new()
        .prepare(&[candidate], 12, profile_id())
        .unwrap();
    let value: serde_json::Value = serde_json::to_value(&batch.entries[0]).unwrap();
    let object = value.as_object().unwrap();
    let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "age_bucket",
            "category_hint",
            "display_name",
            "id",
            "product_hint",
            "relative_depth",
            "signals",
            "size_bucket",
            "source_kind",
            "zone",
        ]
    );
    assert_eq!(object["signals"], serde_json::json!(["path-layout"]));
    assert!(object.get("path").is_none());
    assert!(object.get("evidence").is_none());
    assert!(object.get("explanation").is_none());
}

#[test]
fn sensitive_category_labels_are_replaced_by_a_fixed_placeholder() {
    for category in [ResidueCategory::Credential, ResidueCategory::Session] {
        let mut candidate = item(10, RiskLevel::Review, r"D:\workspace\state");
        candidate.category = category;
        let batch = MetadataSanitizer::new()
            .prepare(&[candidate], 12, profile_id())
            .unwrap();
        let payload = serde_json::to_string(&batch.entries[0]).unwrap();
        assert!(payload.contains("<sensitive-metadata>"));
        assert!(!payload.contains("credential"));
        assert!(!payload.contains("session"));
    }
}

#[test]
fn non_sensitive_hints_are_truncated_without_preserving_path_fragments() {
    let mut candidate = item(9, RiskLevel::Review, r"D:\workspace\ordinary");
    candidate.display_name =
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567890123456789".into();
    candidate.product = Some(
        "product-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567890123456789".into(),
    );

    let batch = MetadataSanitizer::new()
        .prepare(&[candidate], 13, profile_id())
        .unwrap();
    let entry = &batch.entries[0];
    assert_eq!(entry.display_name.chars().count(), 64);
    assert!(entry.display_name.ends_with('…'));
    assert_eq!(entry.product_hint.as_ref().unwrap().chars().count(), 64);
    assert!(entry.product_hint.as_ref().unwrap().ends_with('…'));
}

#[test]
fn tokens_are_random_aliases_and_batch_metadata_is_retained() {
    let items = [item(123, RiskLevel::Unknown, r"C:\Users\alice\cache")];
    let first = MetadataSanitizer::new()
        .prepare(&items, 99, profile_id())
        .unwrap();
    let second = MetadataSanitizer::new()
        .prepare(&items, 99, profile_id())
        .unwrap();

    assert_eq!(first.scan_generation, 99);
    assert_eq!(first.profile_id, profile_id());
    assert_ne!(first.id, second.id);
    assert_ne!(first.entries[0].id.as_str(), "123");
    assert_ne!(first.entries[0].id, second.entries[0].id);
}

#[test]
fn size_and_age_buckets_have_coarse_stable_boundaries() {
    let now = SystemTime::now();
    let paths = [
        r"C:\Users\alice\under-1mib",
        r"C:\Users\alice\under-64mib",
        r"C:\Users\alice\under-1gib",
        r"C:\Users\alice\at-least-1gib",
        r"C:\Users\alice\age-1d",
        r"C:\Users\alice\age-7d",
        r"C:\Users\alice\age-30d",
        r"C:\Users\alice\age-180d",
        r"C:\Users\alice\age-unknown",
    ];
    let mut items = Vec::new();
    for (id, path) in paths.into_iter().enumerate() {
        let mut candidate = item(id as u64, RiskLevel::Unknown, path);
        candidate.logical_size = match id {
            0 => 1_048_575,
            1 => 1_048_576,
            2 => 67_108_864,
            _ => 1_073_741_824,
        };
        candidate.last_modified = match id {
            4 => Some(now - Duration::from_secs(86_400 + 1)),
            5 => Some(now - Duration::from_secs(7 * 86_400 + 1)),
            6 => Some(now - Duration::from_secs(30 * 86_400 + 1)),
            7 => Some(now - Duration::from_secs(180 * 86_400 + 1)),
            8 => None,
            _ => Some(now),
        };
        items.push(candidate);
    }

    let batch = MetadataSanitizer::new()
        .prepare(&items, 10, profile_id())
        .unwrap();
    assert_eq!(batch.entries[0].size_bucket, SizeBucket::Under1MiB);
    assert_eq!(batch.entries[1].size_bucket, SizeBucket::Under64MiB);
    assert_eq!(batch.entries[2].size_bucket, SizeBucket::Under1GiB);
    assert_eq!(batch.entries[3].size_bucket, SizeBucket::AtLeast1GiB);
    assert_eq!(batch.entries[4].age_bucket, AgeBucket::Under7Days);
    assert_eq!(batch.entries[5].age_bucket, AgeBucket::Under30Days);
    assert_eq!(batch.entries[6].age_bucket, AgeBucket::Under180Days);
    assert_eq!(batch.entries[7].age_bucket, AgeBucket::AtLeast180Days);
    assert_eq!(batch.entries[8].age_bucket, AgeBucket::Unknown);
}

#[test]
fn zone_is_a_fixed_label_and_depth_is_capped_without_path_output() {
    let mut deep = item(
        7,
        RiskLevel::Unknown,
        r"C:\Users\alice\AppData\Local\one\two\three\four\five\six\seven\eight\nine",
    );
    deep.display_name = "ordinary cache".into();
    deep.product = None;
    let batch = MetadataSanitizer::new()
        .prepare(&[deep], 11, profile_id())
        .unwrap();

    assert_eq!(batch.entries[0].zone, AiZone::LocalAppData);
    assert_eq!(batch.entries[0].relative_depth, 8);
    let payload = serde_json::to_string(&batch.entries[0]).unwrap();
    assert!(!payload.contains("alice"));
    assert!(!payload.contains("\"one\""));
}

#[test]
fn future_last_modified_is_safely_classified_as_under_one_day() {
    let mut candidate = item(11, RiskLevel::Unknown, r"D:\workspace\clock-skew");
    candidate.last_modified = Some(SystemTime::now() + Duration::from_secs(60));
    let batch = MetadataSanitizer::new()
        .prepare(&[candidate], 14, profile_id())
        .unwrap();
    assert_eq!(batch.entries[0].age_bucket, AgeBucket::Under1Day);
}
