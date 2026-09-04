from voice_recorder.stitch import stitch_chunks


def test_stitch_removes_word_overlap() -> None:
    chunks = [
        "the quick brown fox jumps",
        "fox jumps over the lazy dog",
    ]
    assert stitch_chunks(chunks) == "the quick brown fox jumps over the lazy dog"


def test_stitch_is_tolerant_of_punctuation_and_case() -> None:
    chunks = [
        "we deploy to Azure Container Apps",
        "Container apps, using managed identity",
    ]
    assert stitch_chunks(chunks) == ("we deploy to Azure Container Apps using managed identity")


def test_stitch_does_not_merge_on_short_stopword() -> None:
    # A single common short word must not cause an accidental merge.
    chunks = ["I need to", "to finish the report"]
    result = stitch_chunks(chunks)
    assert result == "I need to to finish the report"


def test_stitch_merges_single_long_word() -> None:
    chunks = ["configure the deployment", "deployment pipeline now"]
    assert stitch_chunks(chunks) == "configure the deployment pipeline now"


def test_stitch_handles_empty_and_single() -> None:
    assert stitch_chunks([]) == ""
    assert stitch_chunks(["", "hello world", ""]) == "hello world"


def test_stitch_no_overlap_concatenates() -> None:
    chunks = ["alpha beta", "gamma delta"]
    assert stitch_chunks(chunks) == "alpha beta gamma delta"
