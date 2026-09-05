import pytest

from voice_recorder.config import Settings


def test_test_environment_allows_minimal_config() -> None:
    settings = Settings(environment="test")
    assert settings.uses_managed_identity is True
    assert settings.transcribe_provider == "azure_speech"
    assert settings.speech_model == "MAI-Transcribe-2"


def test_production_requires_storage_account() -> None:
    with pytest.raises(ValueError, match="storage_account_name"):
        Settings(
            environment="production",
            google_allowed_audiences=("aud",),
            allowlisted_email="a@b.com",
            webpubsub_endpoint="https://x.webpubsub.azure.com",
            speech_endpoint="https://speech.example.com",
        )


def test_production_requires_auth_config() -> None:
    with pytest.raises(ValueError, match="google_allowed_audiences"):
        Settings(
            environment="production",
            storage_account_name="acct",
            webpubsub_endpoint="https://x.webpubsub.azure.com",
            speech_endpoint="https://speech.example.com",
        )


def test_production_forbids_azurite() -> None:
    with pytest.raises(ValueError, match="azurite"):
        Settings(
            environment="production",
            storage_account_name="acct",
            storage_use_azurite=True,
            google_allowed_audiences=("aud",),
            allowlisted_email="a@b.com",
            webpubsub_endpoint="https://x.webpubsub.azure.com",
            speech_endpoint="https://speech.example.com",
        )


def test_production_forbids_fake_ai() -> None:
    with pytest.raises(ValueError, match="fake"):
        Settings(
            environment="production",
            storage_account_name="acct",
            google_allowed_audiences=("aud",),
            allowlisted_email="a@b.com",
            webpubsub_endpoint="https://x.webpubsub.azure.com",
            use_fake_ai=True,
            speech_endpoint="https://speech.example.com",
        )


def test_production_requires_webpubsub() -> None:
    with pytest.raises(ValueError, match="webpubsub_endpoint"):
        Settings(
            environment="production",
            storage_account_name="acct",
            google_allowed_audiences=("aud",),
            allowlisted_email="a@b.com",
            speech_endpoint="https://speech.example.com",
        )


def test_audiences_split_from_csv() -> None:
    settings = Settings(
        environment="test",
        google_allowed_audiences="a.apps.googleusercontent.com, b.apps.googleusercontent.com",
    )
    assert settings.google_allowed_audiences == (
        "a.apps.googleusercontent.com",
        "b.apps.googleusercontent.com",
    )


def test_glossary_prompt_includes_terms() -> None:
    settings = Settings(environment="test")
    prompt = settings.glossary_prompt()
    for term in [
        "Azure",
        "Entra",
        "Copilot",
        "Container Apps",
        "Managed Identity",
        "Backend",
        "MAI-Transcribe-2",
        "REST API",
    ]:
        assert term in prompt
        assert term in settings.glossary_phrases()


def test_speech_provider_requires_endpoint_in_production() -> None:
    with pytest.raises(ValueError, match="speech_endpoint"):
        Settings(
            environment="production",
            storage_account_name="acct",
            google_allowed_audiences=("aud",),
            allowlisted_email="a@b.com",
            webpubsub_endpoint="https://x.webpubsub.azure.com",
            transcribe_provider="azure_speech",
            speech_endpoint="",
        )
