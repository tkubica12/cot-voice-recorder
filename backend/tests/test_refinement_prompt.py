from voice_recorder.prompts import (
    REFINEMENT_SYSTEM_PROMPT,
    build_refinement_messages,
    build_transcription_prompt,
)


def test_refinement_prompt_states_all_invariants() -> None:
    text = REFINEMENT_SYSTEM_PROMPT.lower()
    # Preserve invariants.
    assert "preserve the speaker's original language" in text
    assert "meaning" in text and "decisions" in text and "uncertainty" in text
    # Cleanup invariants.
    assert "filler" in text and "stutters" in text
    assert "abandoned" in text and ("replaced" in text)
    assert "duplication" in text and "overlap" in text
    assert "punctuation" in text
    # Prohibitions.
    assert "summarize" in text
    assert "invent" in text
    assert "answer" in text
    assert "commentary" in text or "meta" in text


def test_refinement_messages_wrap_transcript() -> None:
    messages = build_refinement_messages("uh the the report is done")
    assert messages[0]["role"] == "system"
    assert messages[0]["content"] == REFINEMENT_SYSTEM_PROMPT
    assert messages[1]["role"] == "user"
    assert "<transcript>" in messages[1]["content"]
    assert "uh the the report is done" in messages[1]["content"]


def test_transcription_prompt_contains_glossary() -> None:
    prompt = build_transcription_prompt("Domain glossary: Azure, Entra, Copilot.")
    assert "Azure" in prompt and "Entra" in prompt and "Copilot" in prompt
