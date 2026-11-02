use std::collections::HashSet;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use time::OffsetDateTime;
use voiceprompt_client_protocol::auth::*;
use voiceprompt_client_protocol::realtime::*;

fn config() -> DesktopClientConfig {
    DesktopClientConfig::parse(
        br#"{"installed":{"client_id":"fixture-id","client_secret":"fixture-secret"}}"#,
    )
    .unwrap()
}

#[test]
fn pkce_matches_rfc7636_and_never_repeats_os_random_values() {
    assert_eq!(
        PkcePair::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap(),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let mut verifiers = HashSet::new();
    let mut states = HashSet::new();
    for _ in 0..64 {
        let pair = PkcePair::create().unwrap();
        assert!((43..=128).contains(&pair.verifier.len()));
        assert!(!pair.challenge.contains(['=', '+', '/']));
        assert!(verifiers.insert(pair.verifier));
        assert!(states.insert(pair.state));
    }
    assert!(PkcePair::from_entropy(&[0; 16], &[0; 16]).is_err());
    assert!(PkcePair::from_entropy(&[0; 32], &[]).is_err());
}

#[test]
fn authorization_url_carries_only_public_request_fields() {
    let pair = PkcePair::create().unwrap();
    let uri = authorization_url(
        &config(),
        "http://127.0.0.1:51234/",
        &pair,
        Some("fixture@example.invalid"),
    )
    .unwrap();
    let fields: std::collections::HashMap<_, _> = uri.query_pairs().into_owned().collect();
    assert_eq!(fields["scope"], "openid email profile");
    assert_eq!(fields["access_type"], "offline");
    assert_eq!(fields["prompt"], "consent");
    assert_eq!(fields["code_challenge_method"], "S256");
    assert_eq!(fields["state"], pair.state);
    assert_eq!(fields["login_hint"], "fixture@example.invalid");
    assert!(!uri.as_str().contains("fixture-secret"));
    assert!(!uri.as_str().contains(&pair.verifier));
    for redirect in [
        "https://example.invalid/",
        "http://0.0.0.0:51234/",
        "http://127.0.0.1:51234/?code=x",
    ] {
        assert!(authorization_url(&config(), redirect, &pair, None).is_err());
    }
}

#[test]
fn callbacks_validate_state_and_reject_duplicate_security_fields() {
    assert_eq!(
        validate_callback("?code=abc%2B123&state=expected", "expected").unwrap(),
        "abc+123"
    );
    for query in [
        "?code=abc&state=other",
        "?code=abc",
        "?state=expected",
        "?error=access_denied&state=expected",
        "?code=one&code=two&state=expected",
        "?code=one&state=expected&state=expected",
    ] {
        assert!(validate_callback(query, "expected").is_err());
    }
}

#[test]
fn cache_preserves_csharp_pascal_case_nulls_and_fractional_offsets() {
    let bytes = br#"{"RefreshToken":"r","IdToken":"id","IdTokenExpiresAt":"2026-11-01T10:30:00.1234567+02:00","Email":"fixture@example.invalid"}"#;
    let cache = CachedTokens::parse(bytes).unwrap();
    let roundtrip = cache.encode().unwrap();
    assert_eq!(CachedTokens::parse(&roundtrip).unwrap(), cache);
    let fields: serde_json::Value = serde_json::from_slice(&roundtrip).unwrap();
    assert_eq!(fields["RefreshToken"], "r");
    assert!(fields.get("refresh_token").is_none());
    assert_eq!(CachedTokens::default().encode().unwrap(), b"{}");
    assert_eq!(
        CachedTokens::parse(br#"{"RefreshToken":null,"IdToken":null}"#).unwrap(),
        CachedTokens::default()
    );
    assert!(CachedTokens::parse(br#"{"RefreshToken":123}"#).is_err());
}

#[test]
fn jwt_claims_are_read_only_and_missing_expiry_requires_refresh() {
    let token = format!("e30.{}.", URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000,"email":"fixture@example.invalid","aud":["first","second"],"sub":"s"}"#));
    let claims = JwtClaims::parse(&token).unwrap();
    assert_eq!(claims.audience.as_deref(), Some("first"));
    assert_eq!(claims.expires_at.unwrap().unix_timestamp(), 2_000_000_000);
    let now = OffsetDateTime::from_unix_timestamp(1_999_999_800).unwrap();
    assert!(claims.is_expiring(now, 300).unwrap());
    assert!(!claims.is_expiring(now, 100).unwrap());
    let missing = JwtClaims::parse(&format!("e30.{}.", URL_SAFE_NO_PAD.encode(b"{}"))).unwrap();
    assert!(missing.is_expiring(now, 0).unwrap());
    assert!(claims.is_expiring(now, -1).is_err());
    for token in ["", "not-a-jwt", "a.b.sig", "e30.bm90LWpzb24."] {
        assert!(JwtClaims::parse(token).is_err());
    }
}

#[test]
fn refresh_outcomes_never_promote_an_access_token() {
    for (input, expected) in [
        (
            br#"{"error":"invalid_grant"}"#.as_slice(),
            RefreshOutcome::RefreshRevoked,
        ),
        (br#"{"error":"server_error"}"#, RefreshOutcome::Transient),
        (
            br#"{"error":"temporarily_unavailable"}"#,
            RefreshOutcome::Transient,
        ),
        (
            br#"{"access_token":"not-an-id-token"}"#,
            RefreshOutcome::NoIdToken,
        ),
        (br#"{"id_token":"id"}"#, RefreshOutcome::Success),
    ] {
        assert_eq!(
            TokenResponse::parse(input).unwrap().classify_refresh(),
            expected
        );
    }
}

#[test]
fn sensitive_protocol_objects_and_errors_are_redacted() {
    let secret = "never-log-this-secret";
    let cache = CachedTokens {
        refresh_token: Some(secret.into()),
        ..Default::default()
    };
    assert!(!format!("{cache:?}").contains(secret));
    assert!(!format!("{:?}", config()).contains("fixture-secret"));
    let error =
        CachedTokens::parse(format!(r#"{{"RefreshToken":{{"secret":"{secret}"}}}}"#).as_bytes())
            .unwrap_err();
    assert!(!format!("{error:?} {error}").contains(secret));
}

fn event() -> serde_json::Value {
    json!({"event":"transcript.completed","event_id":"e","transcript_id":"t",
        "recording_id":"r","completed_at":"2026-11-01T10:30:00Z","preview":"synthetic preview"})
}

#[test]
fn pubsub_normalizes_bare_json_and_text_envelopes() {
    for value in [
        event(),
        json!({"type":"message","from":"server","dataType":"json","data":event()}),
        json!({"type":"message","from":"group","dataType":"text","data":event().to_string()}),
    ] {
        let frame = Frame::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(frame.kind, FrameKind::Message);
        let completed = frame.completed().unwrap().unwrap();
        assert_eq!(completed.event_id, "e");
        assert_eq!(completed.transcript_id, "t");
        assert!(!format!("{frame:?} {completed:?}").contains("synthetic preview"));
    }
}

#[test]
fn pubsub_control_frames_and_invalid_payloads_are_explicit() {
    let connected = Frame::parse(br#"{"type":"system","event":"connected"}"#).unwrap();
    assert!(connected.is_connected());
    assert!(!connected.is_disconnected());
    assert!(connected.completed().unwrap().is_none());
    let ack = Frame::parse(br#"{"type":"ack"}"#).unwrap();
    assert_eq!(ack.kind, FrameKind::Ack);
    for data in [
        br#"{"type":"message","dataType":"binary","data":"abc"}"#.as_slice(),
        br#"{"event":"other","event_id":"e","transcript_id":"t"}"#,
        br#"{"event":"transcript.completed","transcript_id":"t"}"#,
    ] {
        assert!(Frame::parse(data).unwrap().completed().unwrap().is_none());
    }
    assert!(Frame::parse(b"bad json").is_err());
    assert!(
        Frame::parse(br#"{"type":"message","dataType":"text","data":"not-json"}"#)
            .unwrap()
            .completed()
            .is_err()
    );
}

#[test]
fn deduplication_is_bounded_fifo_and_case_sensitive() {
    let mut dedupe = EventDeduplicator::new(2).unwrap();
    assert!(dedupe.mark(None).unwrap());
    assert!(dedupe.mark(Some("a")).unwrap());
    assert!(!dedupe.mark(Some("a")).unwrap());
    assert!(dedupe.mark(Some("A")).unwrap());
    assert!(dedupe.mark(Some("b")).unwrap());
    assert!(!dedupe.has_seen("a"));
    assert!(dedupe.has_seen("A"));
    assert_eq!(dedupe.count(), 2);
    dedupe.clear();
    assert_eq!(dedupe.count(), 0);
    assert!(EventDeduplicator::new(0).is_err());
}

#[test]
fn loopback_keeps_its_port_and_validates_before_success_reply() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use voiceprompt_client_protocol::loopback::{LoopbackError, LoopbackListener};
    for (state, status, valid) in [
        ("expected", "200 OK", true),
        ("wrong", "400 Bad Request", false),
    ] {
        let listener = LoopbackListener::bind().unwrap();
        let uri = url::Url::parse(listener.redirect_uri()).unwrap();
        assert_eq!(uri.host_str(), Some("127.0.0.1"));
        let port = uri.port().unwrap();
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(format!("GET /?code=synthetic-code&state={state} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let result =
            listener.wait_for_code("expected", Duration::from_secs(5), &AtomicBool::new(false));
        assert_eq!(result.is_ok(), valid);
        let response = client.join().unwrap();
        assert!(response.contains(status));
        assert!(response.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(response.contains("Cache-Control: no-store\r\n"));
        assert!(response.contains("Referrer-Policy: no-referrer\r\n"));
        assert!(
            response.contains(
                "Content-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\n"
            )
        );
        let (headers, page) = response.split_once("\r\n\r\n").unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(length, page.len());
        assert!(page.contains("<title>VoicePrompt authorization callback</title>"));
        for secret in ["synthetic-code", state, "code=", "state="] {
            assert!(
                !page.contains(secret),
                "The callback page must not echo query material."
            );
        }
    }
    let listener = LoopbackListener::bind().unwrap();
    assert!(matches!(
        listener.wait_for_code("s", Duration::from_secs(1), &AtomicBool::new(true)),
        Err(LoopbackError::Cancelled)
    ));
    assert!(matches!(
        listener.wait_for_code("s", Duration::from_millis(1), &AtomicBool::new(false)),
        Err(LoopbackError::Timeout)
    ));
}

#[test]
fn auth_session_refreshes_only_id_tokens_and_persists_before_publish() {
    use voiceprompt_client_protocol::state::*;
    let now = OffsetDateTime::from_unix_timestamp(1_999_999_800).unwrap();
    let fresh = format!(
        "e30.{}.",
        URL_SAFE_NO_PAD.encode(br#"{"exp":2000010000,"email":"fixture@example.invalid"}"#)
    );
    let mut session = AuthSession::new(
        true,
        Some(CachedTokens {
            refresh_token: Some("old-refresh".into()),
            ..Default::default()
        }),
    );
    assert!(matches!(
        session.token_decision(now, 300).unwrap(),
        TokenDecision::RefreshRequired(RefreshReason::MissingToken)
    ));
    let response = TokenResponse {
        id_token: Some(fresh.clone()),
        refresh_token: Some("rotated".into()),
        ..Default::default()
    };
    assert!(matches!(
        session.apply_refresh(response.clone(), |_| Err("disk failure")),
        Err(TransitionError::Persistence(_))
    ));
    assert!(matches!(
        session.token_decision(now, 300).unwrap(),
        TokenDecision::RefreshRequired(_)
    ));
    session
        .apply_refresh(response, |cache| {
            assert_eq!(cache.refresh_token.as_deref(), Some("rotated"));
            Ok::<(), ()>(())
        })
        .unwrap();
    assert!(
        matches!(session.token_decision(now, 300).unwrap(), TokenDecision::Fresh(value) if value == fresh)
    );
    session
        .apply_refresh(
            TokenResponse {
                error: Some("invalid_grant".into()),
                ..Default::default()
            },
            |cache| {
                assert!(cache.id_token.is_none());
                assert!(cache.id_token_expires_at.is_none());
                Ok::<(), ()>(())
            },
        )
        .unwrap();
    assert_eq!(session.state(), AuthState::SignInNeeded);
    assert!(matches!(
        session.token_decision(now, 300).unwrap(),
        TokenDecision::SignInNeeded
    ));
}

#[test]
fn auth_session_signout_stops_vending_even_when_durable_delete_fails() {
    use voiceprompt_client_protocol::state::*;
    let now = OffsetDateTime::UNIX_EPOCH;
    let mut session = AuthSession::new(
        true,
        Some(CachedTokens {
            refresh_token: Some("r".into()),
            ..Default::default()
        }),
    );
    assert!(matches!(
        session.sign_out(|| Err("delete failure")),
        Err(TransitionError::Persistence(_))
    ));
    assert_eq!(session.state(), AuthState::SignedOut);
    assert!(matches!(
        session.token_decision(now, 300).unwrap(),
        TokenDecision::SignedOut
    ));
    let unconfigured = AuthSession::new(false, None);
    assert!(matches!(
        unconfigured.token_decision(now, 300).unwrap(),
        TokenDecision::NotConfigured
    ));
}
