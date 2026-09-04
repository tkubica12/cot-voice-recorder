import uuid

from fastapi.testclient import TestClient

from voice_recorder.digest import compute_content_digest

from .conftest import chunk_headers, make_wav, unique_wav


def _create(client: TestClient, auth: dict[str, str]) -> str:
    return client.post(
        "/v1/recordings", json={"client_recording_id": str(uuid.uuid4())}, headers=auth
    ).json()["recording_id"]


def _put(
    client: TestClient,
    auth: dict[str, str],
    rid: str,
    index: int,
    data: bytes,
    *,
    headers: dict[str, str] | None = None,
) -> object:
    hdrs = {**auth, **(headers or chunk_headers(data))}
    return client.put(f"/v1/recordings/{rid}/chunks/{index}", content=data, headers=hdrs)


def test_upload_accepts_chunk(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = make_wav()
    resp = _put(client, auth, rid, 0, data)
    assert resp.status_code == 202
    body = resp.json()
    assert body["chunk_state"] == "accepted"
    assert body["index"] == 0
    assert body["checksum"] == compute_content_digest(data)


def test_reupload_same_digest_is_idempotent(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = make_wav()
    assert _put(client, auth, rid, 0, data).status_code == 202
    replay = _put(client, auth, rid, 0, data)
    assert replay.status_code == 200


def test_reupload_different_digest_conflicts(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    assert _put(client, auth, rid, 0, unique_wav(1)).status_code == 202
    resp = _put(client, auth, rid, 0, unique_wav(2))
    assert resp.status_code == 409
    assert resp.json()["code"] == "chunk-conflict"


def test_missing_digest_is_400(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = make_wav()
    resp = _put(client, auth, rid, 0, data, headers={"Content-Type": "audio/wav"})
    assert resp.status_code == 400


def test_malformed_digest_is_422(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = make_wav()
    resp = _put(
        client,
        auth,
        rid,
        0,
        data,
        headers={"Content-Type": "audio/wav", "Content-Digest": "md5=:abc:"},
    )
    assert resp.status_code == 422


def test_digest_mismatch_is_422(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = make_wav()
    wrong = compute_content_digest(b"different")
    resp = _put(
        client,
        auth,
        rid,
        0,
        data,
        headers={"Content-Type": "audio/wav", "Content-Digest": wrong},
    )
    assert resp.status_code == 422


def test_invalid_wav_is_422(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = b"not a wav file at all" + bytes(64)
    resp = _put(
        client,
        auth,
        rid,
        0,
        data,
        headers={"Content-Type": "audio/wav", "Content-Digest": compute_content_digest(data)},
    )
    assert resp.status_code == 422


def test_out_of_range_after_complete_conflicts(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    _put(client, auth, rid, 0, unique_wav(0))
    assert (
        client.post(
            f"/v1/recordings/{rid}/complete", json={"chunk_count": 1}, headers=auth
        ).status_code
        == 202
    )
    # Index 1 is out of range for a declared chunk_count of 1.
    resp = _put(client, auth, rid, 1, unique_wav(1))
    assert resp.status_code == 409
    assert resp.json()["code"] == "chunk-conflict"


def test_upload_to_unknown_recording_404(client: TestClient, auth: dict[str, str]) -> None:
    data = make_wav()
    resp = _put(client, auth, str(uuid.uuid4()), 0, data)
    assert resp.status_code == 404
