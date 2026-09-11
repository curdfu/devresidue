use std::fs;

use devresidue_ai::{AiAuditEvent, AiAuditLog, AiAuditResult, MAX_AUDIT_BYTES};
use devresidue_core::ai::AiProfileId;
use serde_json::Value;

const PROFILE_ID: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";

#[test]
fn audit_never_contains_path_reason_request_response_or_key() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task7-audit-private-{}",
        std::process::id()
    ));
    let audit = AiAuditLog::open(&data_dir);

    audit
        .record(
            AiAuditEvent::ConfirmWritten,
            AiProfileId::parse(PROFILE_ID).unwrap(),
            1,
            AiAuditResult::Ok,
            12,
        )
        .unwrap();

    let text = fs::read_to_string(data_dir.join("ai-audit.jsonl")).unwrap();
    for forbidden in [
        r"C:\Users",
        "because node_modules",
        "Bearer",
        "sk-test-confirm-key",
        "reason",
        "path",
        "\"messages\"",
        "\"choices\"",
        "\"headers\"",
    ] {
        assert!(
            !text.contains(forbidden),
            "audit leaked {forbidden}: {text}"
        );
    }
    let line: Value = serde_json::from_str(text.trim()).unwrap();
    let object = line.as_object().unwrap();
    assert_eq!(object.len(), 7);
    for key in [
        "ts",
        "event",
        "profile_id",
        "count",
        "result",
        "elapsed_ms",
        "request_id",
    ] {
        assert!(object.contains_key(key));
    }
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn audit_retains_only_complete_json_lines_within_its_fixed_capacity() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task7-audit-bounded-{}",
        std::process::id()
    ));
    let audit = AiAuditLog::open(&data_dir);
    let profile_id = AiProfileId::parse(PROFILE_ID).unwrap();

    for count in 0..800 {
        audit
            .record(
                AiAuditEvent::ConfirmWritten,
                profile_id.clone(),
                count,
                AiAuditResult::Ok,
                12,
            )
            .unwrap();
    }

    let text = fs::read_to_string(data_dir.join("ai-audit.jsonl")).unwrap();
    assert!(text.len() <= MAX_AUDIT_BYTES);
    assert!(text.ends_with('\n'));
    assert!(text
        .lines()
        .all(|line| serde_json::from_str::<Value>(line).is_ok()));
    let _ = fs::remove_dir_all(data_dir);
}
