use super::*;
use crate::http_fixture::{config, serve, token};

#[tokio::test]
async fn rest_negotiation_and_delivery_use_configured_user_roles_ttl_and_never_full_text_or_audit()
{
    let mut server = serve(vec![
        token(),
        (200, r#"{"token":"synthetic-client-token"}"#.into()),
        (202, "".into()),
    ])
    .await;
    let mut config = config(&server.endpoint);
    config.group = "fixture-user".into();
    config.hub = "fixture-hub".into();
    config.webpubsub_ttl_minutes = 2;
    let gateway = WebPubSub::new(
        Arc::new(config),
        Arc::new(ManagedIdentity::fixture(server.endpoint.as_str())),
    )
    .unwrap();
    let before = crate::services::now();
    let access = gateway.negotiate().await.unwrap();
    assert_eq!(access.group, "fixture-user");
    assert_eq!(access.hub, "fixture-hub");
    assert!(access.expires_at >= before + time::Duration::minutes(2));
    let client: url::Url = access.url.parse().unwrap();
    assert_eq!(client.scheme(), "wss");
    assert_eq!(client.path(), "/client/hubs/fixture-hub");
    assert!(
        client
            .query_pairs()
            .any(|(key, value)| key == "access_token" && value == "synthetic-client-token")
    );
    let identity = server.next().await;
    assert!(
        identity
            .target
            .query_pairs()
            .any(|(key, value)| key == "resource" && value == "https://webpubsub.azure.com/")
    );
    let request = server.next().await;
    assert_eq!(
        request.target.path(),
        "/api/hubs/fixture-hub/:generateToken"
    );
    let query: std::collections::HashMap<_, _> = request
        .target
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(query["userId"], "fixture-user");
    assert_eq!(query["role"], "webpubsub.joinLeaveGroup.fixture-user");
    assert_eq!(query["group"], "fixture-user");
    assert_eq!(query["minutesToExpire"], "2");
    assert_eq!(query["api-version"], "2024-12-01");
    let services = crate::services::tests::fixture();
    let payload = serde_json::from_value(serde_json::json!({
        "text":"synthetic secret full text","raw_text":"synthetic raw text","completed_at":crate::services::python_datetime(before, true)
    })).unwrap();
    let mut transcript = services
        .archive("0123456789abcdef0123456789abcdef", payload)
        .await
        .unwrap();
    assert_eq!(gateway.notify(&transcript).await.err().unwrap().status, 422);
    transcript.source = "recording".into();
    transcript.preview = "synthetic preview".into();
    gateway.notify(&transcript).await.unwrap();
    let request = server.next().await;
    assert_eq!(
        request.target.path(),
        "/api/hubs/fixture-hub/users/fixture-user/:send"
    );
    let event: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(event["event"], "transcript.completed");
    assert_eq!(event["preview"], "synthetic preview");
    assert!(event.get("body").is_none() && event.get("raw_body").is_none());
    assert!(!String::from_utf8(request.body).unwrap().contains("secret"));
    server.finish().await;
}
