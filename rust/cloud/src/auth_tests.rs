use super::*;
use base64::Engine;
use jsonwebtoken::{EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use std::collections::BTreeMap;
use std::sync::OnceLock;

fn key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap())
}

fn config(uri: &str) -> Arc<Config> {
    let mut values: BTreeMap<String, String> = [
        ("VR_ENVIRONMENT", "test"),
        ("VR_GOOGLE_ALLOWED_AUDIENCES", "synthetic-audience"),
        ("VR_ALLOWLISTED_EMAIL", "fixture@example.invalid"),
        ("VR_STORAGE_ACCOUNT_NAME", "fixture123"),
        (
            "VR_FOUNDRY_ENDPOINT",
            "https://fixture.cognitiveservices.azure.com/",
        ),
        (
            "VR_SPEECH_ENDPOINT",
            "https://fixture.cognitiveservices.azure.com/",
        ),
        (
            "VR_WEBPUBSUB_ENDPOINT",
            "https://fixture.webpubsub.azure.com/",
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    values.insert("VR_GOOGLE_JWKS_URI".into(), uri.into());
    Arc::new(Config::parse(&values).unwrap())
}
fn public() -> (String, String) {
    let key = RsaPublicKey::from(key());
    let base64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    (
        base64.encode(key.n().to_bytes_be()),
        base64.encode(key.e().to_bytes_be()),
    )
}
fn sign(claims: &serde_json::Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("synthetic-key".into());
    let der = key().to_pkcs1_der().unwrap();
    encode(&header, claims, &EncodingKey::from_rsa_der(der.as_bytes())).unwrap()
}
fn claims() -> serde_json::Value {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    serde_json::json!({
        "iss":"https://accounts.google.com","aud":"synthetic-audience","exp":now+3600,
        "iat":now-10,"sub":"synthetic-subject","email":"fixture@example.invalid","email_verified":true,
    })
}

#[test]
fn real_rs256_verification_requires_claims_signature_issuer_audience_and_verified_allowlist() {
    let verifier =
        GoogleVerifier::new(config("https://www.googleapis.com/oauth2/v3/certs")).unwrap();
    let (n, e) = public();
    let key = DecodingKey::from_rsa_components(&n, &e).unwrap();
    let valid = claims();
    assert_eq!(
        verifier.verify_key(&sign(&valid), &key).unwrap().subject,
        "synthetic-subject"
    );
    for field in ["exp", "iat", "iss", "aud"] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove(field);
        assert_eq!(
            verifier
                .verify_key(&sign(&invalid), &key)
                .err()
                .unwrap()
                .status,
            401
        );
    }
    for (field, value, status) in [
        (
            "iss",
            serde_json::json!("https://attacker.example.invalid"),
            401,
        ),
        ("aud", serde_json::json!("wrong-audience"), 401),
        (
            "iat",
            serde_json::json!(time::OffsetDateTime::now_utc().unix_timestamp() + 600),
            401,
        ),
        (
            "nbf",
            serde_json::json!(time::OffsetDateTime::now_utc().unix_timestamp() + 600),
            401,
        ),
        (
            "exp",
            serde_json::json!(time::OffsetDateTime::now_utc().unix_timestamp() - 1),
            401,
        ),
        ("email_verified", serde_json::json!(false), 403),
        ("email_verified", serde_json::json!("true"), 403),
        ("email", serde_json::json!("another@example.invalid"), 403),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert_eq!(
            verifier
                .verify_key(&sign(&invalid), &key)
                .err()
                .unwrap()
                .status,
            status
        );
    }
    let mut damaged = sign(&valid).into_bytes();
    let middle = damaged.iter().rposition(|byte| *byte == b'.').unwrap() + 10;
    damaged[middle] = if damaged[middle] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        verifier
            .verify_key(std::str::from_utf8(&damaged).unwrap(), &key)
            .err()
            .unwrap()
            .status,
        401
    );
}

#[tokio::test]
async fn actual_local_jwks_fetch_is_cached_and_unknown_keys_do_not_trigger_an_unbounded_fetch_loop()
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (n, e) = public();
    let body = serde_json::json!({"keys":[{"kid":"synthetic-key","kty":"RSA","alg":"RS256","use":"sig","n":n,"e":e}]}).to_string();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let verifier = GoogleVerifier::new(config(&format!("http://127.0.0.1:{port}/"))).unwrap();
    let token = sign(&claims());
    assert_eq!(
        verifier.verify(&token).await.unwrap().email,
        "fixture@example.invalid"
    );
    assert_eq!(
        verifier.verify(&token).await.unwrap().audience,
        "synthetic-audience"
    );
    assert!(verifier.key("unknown-key").await.is_err());
    assert!(verifier.key("another-unknown-key").await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn hs256_or_unsigned_tokens_are_rejected_before_network_access() {
    let verifier = GoogleVerifier::new(config("http://127.0.0.1:1/")).unwrap();
    let wrong = encode(
        &Header::new(Algorithm::HS256),
        &claims(),
        &EncodingKey::from_secret(b"synthetic-secret-for-test-only"),
    )
    .unwrap();
    assert_eq!(verifier.verify(&wrong).await.err().unwrap().status, 401);
    assert_eq!(
        verifier
            .verify("eyJhbGciOiJub25lIn0.e30.")
            .await
            .err()
            .unwrap()
            .status,
        401
    );
    assert!(verifier.cache.lock().await.fetched.is_none());
}
