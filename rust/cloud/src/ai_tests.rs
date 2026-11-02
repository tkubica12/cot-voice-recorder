use super::*;
use crate::http_fixture::{config, part, serve, token};

#[tokio::test]
async fn speech_uses_exact_wav_and_multipart_hints_clean_dictation_and_managed_identity() {
    let mut server = serve(vec![
        token(),
        (
            200,
            r#"{"combinedPhrases":[{"text":"  first "},{"text":""},{"text":"second  "}]}"#.into(),
        ),
    ])
    .await;
    let config = Arc::new(config(&server.endpoint));
    let ai = AzureAi::new(
        config,
        Arc::new(ManagedIdentity::fixture(server.endpoint.as_str())),
    )
    .unwrap();
    let wav = voiceprompt_core::wav::to_wav(&vec![12; 640]).unwrap();
    let result = ai
        .transcribe(
            wav.clone(),
            TranscriptionHints {
                language: "cs".into(),
                prompt: String::new(),
                phrases: vec!["synthetic term".into()],
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(result, "first second");
    let identity = server.next().await;
    assert!(
        identity.target.query_pairs().any(
            |(key, value)| key == "resource" && value == "https://cognitiveservices.azure.com/"
        )
    );
    let request = server.next().await;
    assert_eq!(
        request.target.path(),
        "/speechtotext/transcriptions:transcribe"
    );
    assert!(
        request
            .target
            .query_pairs()
            .any(|(key, value)| key == "api-version" && value == "2025-10-15")
    );
    assert!(
        request
            .headers
            .to_lowercase()
            .contains("authorization: bearer fixture-data-plane")
    );
    assert!(
        request
            .headers
            .to_lowercase()
            .contains("multipart/form-data; boundary=")
    );
    assert_eq!(part(&request.body, "audio"), wav);
    let definition: serde_json::Value =
        serde_json::from_slice(part(&request.body, "definition")).unwrap();
    assert_eq!(
        definition,
        serde_json::json!({"locales":["cs"],"phraseList":{"phrases":["synthetic term"]},
        "enhancedMode":{"enabled":true,"model":"MAI-Transcribe-2","modelOptions":{"transcribeStyle":"clean"}}})
    );
    server.finish().await;
}

#[tokio::test]
async fn speech_rejects_malformed_phrases_and_distinguishes_terminal_and_transient_failures() {
    let mut server = serve(vec![
        token(),
        (200, r#"{"combinedPhrases":[{"text":5}]}"#.into()),
        (400, "provider body must not leak".into()),
        (429, "provider body must not leak".into()),
    ])
    .await;
    let ai = AzureAi::new(
        Arc::new(config(&server.endpoint)),
        Arc::new(ManagedIdentity::fixture(server.endpoint.as_str())),
    )
    .unwrap();
    for status in [502, 502, 503] {
        let error = ai
            .transcribe(
                vec![0; 44],
                TranscriptionHints {
                    language: "auto".into(),
                    prompt: String::new(),
                    phrases: vec![],
                },
                true,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.status, status);
        assert!(!error.detail.contains("provider body"));
    }
    server.next().await;
    for _ in 0..3 {
        let request = server.next().await;
        let definition: serde_json::Value =
            serde_json::from_slice(part(&request.body, "definition")).unwrap();
        assert!(definition.get("locales").is_none());
        assert!(definition.get("phraseList").is_none());
    }
    server.finish().await;
}

#[tokio::test]
async fn openai_legacy_provider_preserves_deployment_prompt_and_terminal_failures() {
    let mut server = serve(vec![
        token(),
        (200, "synthetic text".into()),
        (403, "private provider response".into()),
    ])
    .await;
    let mut config = config(&server.endpoint);
    config.transcribe_provider = "azure_openai".into();
    config.speech = None;
    let ai = AzureAi::new(
        Arc::new(config),
        Arc::new(ManagedIdentity::fixture(server.endpoint.as_str())),
    )
    .unwrap();
    let hints = || TranscriptionHints {
        language: "en".into(),
        prompt: "synthetic glossary".into(),
        phrases: vec![],
    };
    assert_eq!(
        ai.transcribe(vec![0; 44], hints(), false).await.unwrap(),
        "synthetic text"
    );
    assert_eq!(
        ai.transcribe(vec![0; 44], hints(), false)
            .await
            .err()
            .unwrap()
            .status,
        502
    );
    assert_eq!(
        ai.transcribe(vec![0; 44], hints(), true)
            .await
            .err()
            .unwrap()
            .status,
        503
    );
    server.next().await;
    for _ in 0..2 {
        let request = server.next().await;
        assert_eq!(
            request.target.path(),
            "/openai/deployments/gpt-4o-transcribe/audio/transcriptions"
        );
        assert_eq!(part(&request.body, "prompt"), b"synthetic glossary");
        assert_eq!(part(&request.body, "language"), b"en");
        assert_eq!(part(&request.body, "model"), b"gpt-4o-transcribe");
        assert_eq!(part(&request.body, "response_format"), b"text");
    }
    server.finish().await;
}

#[tokio::test]
async fn refinement_keeps_exact_prompt_and_untrusted_text_then_rejects_incomplete_or_unsafe_edits()
{
    let completion = |content: &str, finish: &str| {
        serde_json::json!({"choices":[{"finish_reason":finish, "message":{"content":content,"refusal":null}}]}).to_string()
    };
    let mut server = serve(vec![
        token(),
        (
            200,
            completion(r#"{"edits":[["current"," is is"," is"]]}"#, "stop"),
        ),
        (200, completion(r#"{"edits":[]}"#, "length")),
        (
            200,
            completion(r#"{"edits":[["current","8443","9443"]]}"#, "stop"),
        ),
    ])
    .await;
    let ai = AzureAi::new(
        Arc::new(config(&server.endpoint)),
        Arc::new(ManagedIdentity::fixture(server.endpoint.as_str())),
    )
    .unwrap();
    assert_eq!(
        ai.refine_dictation("This is is a test.".into(), "prior synthetic text".into())
            .await
            .unwrap()
            .text,
        "This is a test."
    );
    assert_eq!(
        ai.refine_dictation("hello".into(), "".into())
            .await
            .err()
            .unwrap()
            .status,
        502
    );
    assert_eq!(
        ai.refine_dictation("port 8443".into(), "".into())
            .await
            .err()
            .unwrap()
            .status,
        502
    );
    server.next().await;
    let request = server.next().await;
    assert_eq!(
        request.target.path(),
        "/openai/deployments/gpt-6-luna/chat/completions"
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        DICTATION_PROMPT
            .replace("\r\n", "\n")
            .trim_end_matches('\n')
    );
    let data: serde_json::Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        data,
        serde_json::json!({"current":"This is is a test.", "previous":"prior synthetic text"})
    );
    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(
        body["response_format"],
        serde_json::json!({"type":"json_object"})
    );
    assert_eq!(body["max_completion_tokens"], 2048);
    server.finish().await;
}

#[tokio::test]
async fn cancelled_http_waiter_keeps_physical_slot_until_work_finishes() {
    let pool = Arc::new(ExecutionPool::new(1, Duration::from_secs(10)));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let waiting = release.clone();
    let running = started.clone();
    let worker = pool.clone();
    let waiter = tokio::spawn(async move {
        worker
            .execute(async move {
                running.notify_one();
                waiting.notified().await;
                Ok(())
            })
            .await
    });
    started.notified().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(
        pool.execute(async { Ok(()) }).await.err().unwrap().status,
        429
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while pool.slots.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(pool.execute(async { Ok(()) }).await.is_ok());
}

#[tokio::test]
async fn whole_recording_refiner_preserves_its_own_original_prompt_and_selected_deployment() {
    let mut server = serve(vec![
        token(),
        (
            200,
            r#"{"choices":[{"message":{"content":"synthetic refined text"}}]}"#.into(),
        ),
    ])
    .await;
    let ai = AzureAi::new(
        Arc::new(config(&server.endpoint)),
        Arc::new(ManagedIdentity::fixture(server.endpoint.as_str())),
    )
    .unwrap();
    assert_eq!(
        ai.refine_recording("synthetic raw text", "gpt-5.6-terra")
            .await
            .unwrap(),
        "synthetic refined text"
    );
    server.next().await;
    let request = server.next().await;
    assert_eq!(
        request.target.path(),
        "/openai/deployments/gpt-5.6-terra/chat/completions"
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "gpt-5.6-terra");
    assert_eq!(
        body["messages"][0]["content"],
        RECORDING_PROMPT
            .replace("\r\n", "\n")
            .trim_end_matches('\n')
    );
    assert_eq!(
        body["messages"][1]["content"],
        "Clean up the following raw transcript according to the rules. Return only the cleaned transcript.\n\n<transcript>\nsynthetic raw text\n</transcript>"
    );
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("response_format").is_none());
    server.finish().await;
}
