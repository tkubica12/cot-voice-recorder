package com.tomaskubica.voiceprompt.data.model

/** Local lifecycle of a recording as tracked on-device (distinct from server state). */
enum class LocalRecordingState {
    RECORDING,          // capture in progress
    STOPPED,            // capture finished; chunks uploading; complete not yet requested/acked
    COMPLETING,         // complete requested / in flight
    COMPLETED,          // backend acknowledged completion
    FAILED,             // terminal error (visible, retryable by user)
    ;
}

/** Last known server-side state (mirrors RecordingState in the OpenAPI contract). */
enum class ServerRecordingState {
    UNKNOWN, RECORDING, UPLOADING, TRANSCRIBING, REFINING, COMPLETED, FAILED;

    companion object {
        fun fromWire(value: String?): ServerRecordingState = when (value) {
            "recording" -> RECORDING
            "uploading" -> UPLOADING
            "transcribing" -> TRANSCRIBING
            "refining" -> REFINING
            "completed" -> COMPLETED
            "failed" -> FAILED
            else -> UNKNOWN
        }
    }
}

enum class ChunkUploadState { PENDING, ACKED, FAILED }

/** Allowed refinement models (RefineModel in the contract). */
enum class RefineModel(val wire: String) {
    LUNA("gpt-6-luna"),
    LEGACY_LUNA("gpt-5.6-luna"),
    TERRA("gpt-5.6-terra");

    companion object {
        val DEFAULT = LUNA
    }
}
