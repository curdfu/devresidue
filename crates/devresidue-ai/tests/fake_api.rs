use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use devresidue_ai::{AiCancellationToken, OpenAiCompatibleTransport};
use devresidue_core::ai::{
    AgeBucket, AiBatchId, AiEntryToken, AiProfile, AiProfileId, AiZone, PreparedBatch,
    SanitizedEntry, SizeBucket, StructuredOutputMode,
};
use devresidue_core::RiskLevel;
use serde_json::json;

const PROFILE_ID: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";
const TOKEN: &str = "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f";

struct FakeApi {
    base_url: String,
    request: mpsc::Receiver<String>,
}

struct FakeResponse {
    status: u16,
    body: String,
    headers: Vec<(&'static str, String)>,
    delay: Duration,
}

impl FakeResponse {
    fn new(status: u16, body: String) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

impl FakeApi {
    fn success(content: impl AsRef<str>) -> Self {
        Self::responses(vec![(200, chat_completion_body(content.as_ref()))])
    }

    fn responses(responses: Vec<(u16, String)>) -> Self {
        Self::response_specs(
            responses
                .into_iter()
                .map(|(status, body)| FakeResponse::new(status, body))
                .collect(),
        )
    }

    fn response_specs(responses: Vec<FakeResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, request) = mpsc::channel();
        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request_text = read_request(&mut stream);
                sender.send(request_text).unwrap();
                if !response.delay.is_zero() {
                    thread::sleep(response.delay);
                }
                write_response(&mut stream, &response);
            }
        });
        Self {
            base_url: format!("http://localhost:{}/v1", address.port()),
            request,
        }
    }

    fn received_requests(&self, count: usize) -> Vec<String> {
        (0..count)
            .map(|_| self.request.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect()
    }
}

fn chat_completion_body(content: &str) -> String {
    chat_completion_body_with_content(json!(content))
}

fn chat_completion_body_with_content(content: serde_json::Value) -> String {
    json!({"choices": [{"message": {"content": content}}]}).to_string()
}

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        bytes.extend_from_slice(&buffer[..read]);
        let Some(headers_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..headers_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default();
        if bytes.len() >= headers_end + 4 + content_length {
            return String::from_utf8(bytes).unwrap();
        }
    }
}

fn write_response(stream: &mut TcpStream, response: &FakeResponse) {
    write!(
        stream,
        "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        response.status,
        response.body.len()
    )
    .unwrap();
    for (name, value) in &response.headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "Connection: close\r\n\r\n{}", response.body).unwrap();
    stream.flush().unwrap();
}

fn header_value<'a>(request: &'a str, expected_name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(expected_name)
            .then_some(value.trim())
    })
}

fn profile(base_url: String) -> AiProfile {
    profile_with_mode(base_url, 5, StructuredOutputMode::JsonSchema)
}

fn profile_with_timeout(base_url: String, timeout_secs: u64) -> AiProfile {
    profile_with_mode(base_url, timeout_secs, StructuredOutputMode::JsonSchema)
}

fn profile_with_mode(
    base_url: String,
    timeout_secs: u64,
    structured_output_mode: StructuredOutputMode,
) -> AiProfile {
    AiProfile::new(
        AiProfileId::parse(PROFILE_ID).unwrap(),
        "local test endpoint".to_string(),
        base_url,
        "test-model".to_string(),
        structured_output_mode,
        timeout_secs,
        true,
    )
}

fn batch() -> PreparedBatch {
    PreparedBatch {
        id: AiBatchId::new("7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d".to_string()),
        profile_id: AiProfileId::parse(PROFILE_ID).unwrap(),
        scan_generation: 42,
        entries: vec![SanitizedEntry {
            id: AiEntryToken::new(TOKEN.to_string()),
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
fn strict_schema_request_returns_only_validated_in_memory_suggestions() {
    let content = r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#;
    let api = FakeApi::success(content);
    let batch = batch();
    let transport = OpenAiCompatibleTransport::new();

    let suggestions = transport
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch,
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].suggested_risk, RiskLevel::Review);
    assert_eq!(suggestions[0].profile_id, batch.profile_id);
    assert_eq!(suggestions[0].scan_generation, batch.scan_generation);

    let request = api.received_requests(1).pop().unwrap();
    assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert_eq!(
        header_value(&request, "authorization"),
        Some("Bearer sk-test-transport-key")
    );
    assert!(request.contains("\"temperature\":0"));
    assert!(request.contains("\"json_schema\""));
    assert!(request.contains("For every input id"));
    assert!(request.contains("Use Simplified Chinese for every human-readable explanation"));
    assert!(request.contains("access credentials, authentication secrets, API keys"));
    assert!(!request.contains("credentials, tokens, keys"));
    let request_body: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let user_content = request_body["messages"][1]["content"].as_str().unwrap();
    let outbound_entries: serde_json::Value = serde_json::from_str(user_content).unwrap();
    assert_eq!(outbound_entries[0]["display_name"], json!("cache"));
    assert_eq!(
        request_body["response_format"]["json_schema"]["schema"]["properties"]["suggestions"]
            ["minItems"],
        json!(1)
    );
    assert_eq!(
        request_body["response_format"]["json_schema"]["schema"]["properties"]["suggestions"]
            ["maxItems"],
        json!(1)
    );
}

#[test]
fn strict_schema_binds_suggestion_count_to_the_current_batch_size() {
    let second_token = "c0ffee00-0000-4000-8000-000000000002";
    let api = FakeApi::success(
        json!({
            "suggestions": [
                {
                    "id": TOKEN,
                    "suggested_risk": "review",
                    "confidence": 0.8,
                    "reason": "Likely local cache.",
                    "product_guess": null
                },
                {
                    "id": second_token,
                    "suggested_risk": "review",
                    "confidence": 0.7,
                    "reason": "Likely package cache.",
                    "product_guess": null
                }
            ]
        })
        .to_string(),
    );
    let mut prepared_batch = batch();
    prepared_batch.entries.push(SanitizedEntry {
        id: AiEntryToken::new(second_token.to_string()),
        zone: AiZone::LocalAppData,
        relative_depth: 2,
        display_name: "package-cache".to_string(),
        source_kind: "unknown-provider".to_string(),
        category_hint: "unknown".to_string(),
        product_hint: None,
        size_bucket: SizeBucket::Under64MiB,
        age_bucket: AgeBucket::Under30Days,
        signals: vec!["path-layout".to_string()],
    });

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &prepared_batch,
            &AiCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(suggestions.len(), 2);

    let request = api.received_requests(1).pop().unwrap();
    let request_body: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let suggestions_schema =
        &request_body["response_format"]["json_schema"]["schema"]["properties"]["suggestions"];
    assert_eq!(suggestions_schema["minItems"], json!(2));
    assert_eq!(suggestions_schema["maxItems"], json!(2));
}

#[test]
fn text_content_blocks_are_validated_like_string_content() {
    let content = r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#;
    let api = FakeApi::responses(vec![(
        200,
        chat_completion_body_with_content(json!([
            {"type": "text", "text": content}
        ])),
    )]);

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch(),
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].suggested_risk, RiskLevel::Review);
}

#[test]
fn one_json_markdown_fence_is_unwrapped_before_strict_validation() {
    let api = FakeApi::success(
        r#"```json
{
    "suggestions": [{
        "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
        "suggested_risk": "review",
        "confidence": 0.8,
        "reason": "Likely local cache.",
        "product_guess": null
    }]
}
```"#,
    );

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch(),
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].suggested_risk, RiskLevel::Review);
}

#[test]
fn connection_test_uses_a_minimal_authenticated_request_without_scan_metadata() {
    let api = FakeApi::responses(vec![(200, "{}".to_string())]);

    OpenAiCompatibleTransport::new()
        .test_connection(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &AiCancellationToken::new(),
        )
        .unwrap();

    let request = api.received_requests(1).pop().unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert_eq!(
        header_value(&request, "authorization"),
        Some("Bearer sk-test-transport-key")
    );
    assert!(!request.contains("display_name"));
    assert!(!request.contains("suggestions"));
    assert!(!request.contains("content-type"));
}

#[test]
fn explicitly_unsupported_schema_falls_back_once_to_json_object() {
    let content = r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#;
    let api = FakeApi::responses(vec![
        (
            400,
            json!({"error": {"code": "response_format_not_supported"}}).to_string(),
        ),
        (200, chat_completion_body(content)),
    ]);
    let batch = batch();

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile_with_mode(api.base_url.clone(), 5, StructuredOutputMode::Auto),
            "sk-test-transport-key",
            &batch,
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    let requests = api.received_requests(2);
    assert!(requests[0].contains("\"json_schema\""));
    assert!(requests[1].contains("\"json_object\""));
    assert!(!requests[1].contains("\"json_schema\""));
}

#[test]
fn ordinary_bad_request_never_falls_back_to_a_second_schema_mode() {
    let api = FakeApi::responses(vec![(
        400,
        json!({"error": {"code": "invalid_request"}}).to_string(),
    )]);

    let error = OpenAiCompatibleTransport::new()
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch(),
            &AiCancellationToken::new(),
        )
        .unwrap_err();

    assert_eq!(error.status(), Some(400));
    assert_eq!(api.received_requests(1).len(), 1);
}

#[test]
fn auto_mode_retries_json_object_after_a_compatible_endpoint_rejects_schema() {
    let content = r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#;
    let api = FakeApi::responses(vec![
        (
            400,
            json!({"error": {"code": "invalid_request"}}).to_string(),
        ),
        (200, chat_completion_body(content)),
    ]);

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile_with_mode(api.base_url.clone(), 5, StructuredOutputMode::Auto),
            "sk-test-transport-key",
            &batch(),
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    let requests = api.received_requests(2);
    assert!(requests[0].contains("\"json_schema\""));
    assert!(requests[1].contains("\"json_object\""));
    assert!(!requests[1].contains("\"json_schema\""));
}

#[test]
fn explicitly_authorized_path_is_sent_only_as_one_structured_entry_field() {
    let api = FakeApi::success(
        r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#,
    );

    OpenAiCompatibleTransport::new()
        .analyze_with_paths(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch(),
            &[PathBuf::from(r"C:\Users\alice\AppData\Local\cache")],
            &AiCancellationToken::new(),
        )
        .unwrap();

    let request = api.received_requests(1).pop().unwrap();
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let user_content = body["messages"][1]["content"].as_str().unwrap();
    let outbound_entries: serde_json::Value = serde_json::from_str(user_content).unwrap();
    assert_eq!(
        outbound_entries[0]["local_path"],
        json!(r"C:\Users\alice\AppData\Local\cache")
    );
    let body_text = body.to_string();
    assert!(!body_text.contains("sk-test-transport-key"));
    assert!(!body_text.contains("\"file_content\""));
    assert!(!body_text.contains("\"credentials\""));
}

#[test]
fn model_list_returns_only_safe_nonempty_model_ids() {
    let api = FakeApi::responses(vec![(
        200,
        json!({
            "data": [
                {"id": "gpt-5.6-luna", "owned_by": "provider"},
                {"id": "gpt-5.6-luna"},
                {"id": ""},
                {"id": "bad\nmodel"},
                {"id": "gpt-4.1-mini"}
            ],
            "object": "list"
        })
        .to_string(),
    )]);

    let models = OpenAiCompatibleTransport::new()
        .list_models(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(models, vec!["gpt-4.1-mini", "gpt-5.6-luna"]);
    let request = api.received_requests(1).pop().unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert!(!request.contains("display_name"));
}

#[test]
fn invalid_model_content_is_rejected_without_echoing_tokens_or_paths() {
    let unknown_token = json!({
        "suggestions": [{
            "id": "c0ffee00-0000-4000-8000-000000000001",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    })
    .to_string();
    let extra_path = json!({
        "suggestions": [{
            "id": TOKEN,
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null,
            "path": r"C:\private\cache"
        }]
    })
    .to_string();

    for content in [unknown_token, extra_path] {
        let api = FakeApi::success(&content);
        let error = OpenAiCompatibleTransport::new()
            .analyze(
                &profile(api.base_url.clone()),
                "sk-test-transport-key",
                &batch(),
                &AiCancellationToken::new(),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            devresidue_ai::AiServiceErrorKind::InvalidResponse
        );
        assert!(!format!("{error:?}").contains("c0ffee00"));
        assert!(!format!("{error:?}").contains(r"C:\private\cache"));
        assert_eq!(api.received_requests(1).len(), 1);
    }
}

#[test]
fn transient_503_is_retried_before_the_batch_fails() {
    let content = r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#;
    let api = FakeApi::responses(vec![
        (503, "{}".to_string()),
        (200, chat_completion_body(content)),
    ]);
    let batch = batch();

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch,
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert_eq!(api.received_requests(2).len(), 2);
}

#[test]
fn retry_after_is_honored_for_transient_rate_limits() {
    let content = r#"{
        "suggestions": [{
            "id": "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            "suggested_risk": "review",
            "confidence": 0.8,
            "reason": "Likely local cache.",
            "product_guess": null
        }]
    }"#;
    let api = FakeApi::response_specs(vec![
        FakeResponse::new(429, "{}".to_string()).header("Retry-After", "1"),
        FakeResponse::new(200, chat_completion_body(content)),
    ]);
    let start = Instant::now();

    let suggestions = OpenAiCompatibleTransport::new()
        .analyze(
            &profile(api.base_url.clone()),
            "sk-test-transport-key",
            &batch(),
            &AiCancellationToken::new(),
        )
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert_eq!(api.received_requests(2).len(), 2);
}

#[test]
fn authentication_and_validation_failures_are_not_retried_or_leaked() {
    for status in [401, 422] {
        let api = FakeApi::response_specs(vec![FakeResponse::new(
            status,
            r#"{"error":{"message":"sk-test-transport-key C:\\private\\cache"}}"#.to_string(),
        )
        .header("X-Request-ID", "sk-test-transport-key")]);
        let error = OpenAiCompatibleTransport::new()
            .analyze(
                &profile(api.base_url.clone()),
                "sk-test-transport-key",
                &batch(),
                &AiCancellationToken::new(),
            )
            .unwrap_err();

        assert_eq!(error.status(), Some(status));
        assert_eq!(error.request_id(), None);
        assert!(!error.to_string().contains("sk-test-transport-key"));
        assert!(!format!("{error:?}").contains("C:\\private"));
        assert_eq!(api.received_requests(1).len(), 1);
    }
}

#[test]
fn total_timeout_bounds_an_inflight_request() {
    let api = FakeApi::response_specs(vec![FakeResponse::new(
        200,
        chat_completion_body(r#"{"suggestions":[]}"#),
    )
    .delayed(Duration::from_secs(2))]);
    let start = Instant::now();

    let error = OpenAiCompatibleTransport::new()
        .analyze(
            &profile_with_timeout(api.base_url.clone(), 1),
            "sk-test-transport-key",
            &batch(),
            &AiCancellationToken::new(),
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        devresidue_ai::AiServiceErrorKind::RequestFailed
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(api.received_requests(1).len(), 1);
}

#[test]
fn cancellation_stops_a_retry_before_a_second_request() {
    let api = FakeApi::response_specs(vec![
        FakeResponse::new(503, "{}".to_string()).delayed(Duration::from_millis(100))
    ]);
    let cancellation = AiCancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let worker_profile = profile(api.base_url.clone());
    let worker_batch = batch();
    let worker = thread::spawn(move || {
        OpenAiCompatibleTransport::new().analyze(
            &worker_profile,
            "sk-test-transport-key",
            &worker_batch,
            &worker_cancellation,
        )
    });

    assert_eq!(api.received_requests(1).len(), 1);
    cancellation.cancel();
    let error = worker.join().unwrap().unwrap_err();

    assert_eq!(error.kind(), devresidue_ai::AiServiceErrorKind::Cancelled);
}
