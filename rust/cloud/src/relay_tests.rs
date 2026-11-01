use super::*;

fn ack(item: &str) -> Value {
    serde_json::json!({"type":"input_audio_buffer.committed", "item_id":item})
}
fn complete(item: &str, text: &str) -> Value {
    serde_json::json!({"type":format!("{PREFIX}completed"), "item_id":item,"transcript":text})
}
#[test]
fn acknowledgements_and_hypotheses_never_confirm_audio_and_completions_are_emitted_in_capture_order()
 {
    let mut state = RelayState::new(1000);
    let audio = state.audio(&[1, 2, 3, 4]).unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(audio["audio"].as_str().unwrap())
            .unwrap(),
        vec![1, 2, 3, 4]
    );
    assert!(state.control(r#"{"type":"commit"}"#).unwrap().is_some());
    state.audio(&[5, 6]).unwrap();
    state.control(r#"{"type":"finish"}"#).unwrap();
    assert!(state.event(ack("first")).unwrap().is_empty());
    assert!(state.event(ack("second")).unwrap().is_empty());
    assert!(state.done().is_none());
    let delta = state.event(serde_json::json!({"type":format!("{PREFIX}delta"),"item_id":"first","delta":"provisional"})).unwrap();
    assert_eq!(delta[0]["type"], "delta");
    assert!(state.done().is_none());
    assert!(
        state
            .event(complete("second", "second text"))
            .unwrap()
            .is_empty()
    );
    let messages = state.event(complete("first", "first text")).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0],
        serde_json::json!({"type":"confirmed","item_id":"first","byte_end":1004,"text":"first text"})
    );
    assert_eq!(messages[1]["byte_end"], 1006);
    assert_eq!(
        state.done().unwrap(),
        serde_json::json!({"type":"done","byte_end":1006})
    );
}
#[test]
fn malformed_duplicate_missing_ack_and_failed_completions_never_produce_done_or_confirmed_text() {
    let mut state = RelayState::new(0);
    assert!(state.audio(&[]).is_err());
    assert!(state.audio(&[1]).is_err());
    assert!(state.audio(&vec![0; 16386]).is_err());
    for control in [
        r#"{"type":"commit","extra":true}"#,
        r#"{"type":"commit","type":"finish"}"#,
        r#"{"type":"wrong"}"#,
    ] {
        assert!(state.control(control).is_err());
    }
    state.audio(&[1, 2]).unwrap();
    state.control(r#"{"type":"finish"}"#).unwrap();
    assert!(state.event(complete("without-ack", "partial")).is_err());
    state.event(ack("first")).unwrap();
    assert!(state.event(ack("first")).is_err());
    assert!(
        state
            .event(serde_json::json!({"type":format!("{PREFIX}failed"),"item_id":"first"}))
            .is_err()
    );
    assert!(state.done().is_none());
    assert!(state.audio(&[1, 2]).is_err());
}
#[test]
fn commit_backpressure_is_bounded_and_empty_commit_never_advances_cursor() {
    let mut state = RelayState::new(0);
    assert!(state.control(r#"{"type":"commit"}"#).unwrap().is_none());
    for _ in 0..32 {
        state.audio(&[1, 2]).unwrap();
        state.control(r#"{"type":"commit"}"#).unwrap();
    }
    state.audio(&[1, 2]).unwrap();
    assert!(state.control(r#"{"type":"commit"}"#).is_err());
    assert!(state.done().is_none());
}
#[test]
fn audio_duration_cap_accepts_the_exact_last_sample_but_rejects_the_next_one() {
    let mut state = RelayState::new(0);
    state.cursor = 32000 * 3600 - 2;
    assert!(state.audio(&[0, 0]).is_ok());
    assert_eq!(state.cursor, 32000 * 3600);
    assert!(state.audio(&[0, 0]).is_err());
}
