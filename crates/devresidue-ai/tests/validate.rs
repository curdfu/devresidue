use devresidue_ai::ResponseValidator;
use devresidue_core::ai::{
    AgeBucket, AiBatchId, AiEntryToken, AiProfileId, AiZone, PreparedBatch, SanitizedEntry,
    SizeBucket,
};
use devresidue_core::RiskLevel;

const PROFILE_ID: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";

fn prepared_batch() -> PreparedBatch {
    PreparedBatch {
        id: AiBatchId::new("7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d".to_string()),
        profile_id: AiProfileId::parse(PROFILE_ID).unwrap(),
        scan_generation: 42,
        entries: vec![SanitizedEntry {
            id: AiEntryToken::new("9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f".to_string()),
            zone: AiZone::LocalAppData,
            relative_depth: 2,
            display_name: "cache".to_string(),
            source_kind: "unknown-provider".to_string(),
            category_hint: "unknown".to_string(),
            product_hint: None,
            size_bucket: SizeBucket::Under64MiB,
            age_bucket: AgeBucket::Under30Days,
            signals: vec!["path-layout".to_string()],
        }],
    }
}

#[test]
fn validator_accepts_a_complete_response_for_the_prepared_batch() {
    let batch = prepared_batch();
    let validator = ResponseValidator::for_batch(&batch);

    let suggestions = validator
        .validate(
            r#"{
                "suggestions": [{
                    "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                    "suggested_risk": "review",
                    "confidence": 0.8,
                    "reason": "Likely local cache.",
                    "product_guess": null
                }]
            }"#,
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].token, batch.entries[0].id);
    assert_eq!(suggestions[0].suggested_risk, RiskLevel::Review);
    assert_eq!(suggestions[0].profile_id, batch.profile_id);
    assert_eq!(suggestions[0].scan_generation, batch.scan_generation);
}

#[test]
fn validator_rejects_unknown_token_duplicate_token_and_extra_path_field() {
    let validator = ResponseValidator::for_batch(&prepared_batch());
    for response in [
        r#"{"suggestions":[{"id":"not-a-registered-token","suggested_risk":"review","confidence":0.8,"reason":"cache","product_guess":null}]}"#,
        r#"{"suggestions":[{"id":"9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f","suggested_risk":"review","confidence":0.8,"reason":"cache","product_guess":null},{"id":"9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f","suggested_risk":"review","confidence":0.8,"reason":"cache","product_guess":null}]}"#,
        r#"{"suggestions":[{"id":"9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f","suggested_risk":"review","confidence":0.8,"reason":"cache","product_guess":null,"path":"C:\\Users\\alice"}]}"#,
    ] {
        assert!(validator.validate(response).is_err());
    }
}

#[test]
fn validator_rejects_out_of_range_confidence_and_redacts_model_text() {
    let validator = ResponseValidator::for_batch(&prepared_batch());
    let invalid = r#"{"suggestions":[{"id":"9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f","suggested_risk":"review","confidence":1.01,"reason":"cache","product_guess":null}]}"#;
    assert!(validator.validate(invalid).is_err());

    let sensitive = r#"{"suggestions":[{"id":"9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f","suggested_risk":"review","confidence":0.8,"reason":"C:\\Users\\alice Bearer sk-test","product_guess":"api key cache"}]}"#;
    let suggestions = validator.validate(sensitive).unwrap();
    assert_eq!(suggestions[0].reason, "<redacted-model-text>");
    assert_eq!(
        suggestions[0].product_guess.as_deref(),
        Some("<redacted-model-text>")
    );
}
