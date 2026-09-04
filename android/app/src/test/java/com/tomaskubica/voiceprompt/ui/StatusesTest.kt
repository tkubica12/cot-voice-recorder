package com.tomaskubica.voiceprompt.ui

import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.data.db.RecordingEntity
import com.tomaskubica.voiceprompt.service.CapturePhase
import org.junit.Test

class StatusesTest {

    private fun recording(
        local: String = "RECORDING",
        server: String = "UNKNOWN",
        retryRequestedAtEpochMs: Long? = null,
    ) = RecordingEntity(
        clientRecordingId = "c",
        localState = local,
        serverState = server,
        refineModel = "gpt-5.6-luna",
        language = "cs",
        retryRequestedAtEpochMs = retryRequestedAtEpochMs,
        startedAtEpochMs = 0,
        createdAtEpochMs = 0,
        updatedAtEpochMs = 0,
    )

    @Test fun recording_phase_wins() {
        assertThat(Statuses.displayStatus(CapturePhase.RECORDING, null, 0)).isEqualTo(DisplayStatus.RECORDING)
    }

    @Test fun stopping_phase_is_uploading() {
        assertThat(Statuses.displayStatus(CapturePhase.STOPPING, null, 0)).isEqualTo(DisplayStatus.UPLOADING)
    }

    @Test fun idle_with_no_recording_is_idle() {
        assertThat(Statuses.displayStatus(CapturePhase.IDLE, null, 0)).isEqualTo(DisplayStatus.IDLE)
    }

    @Test fun completed_server_state() {
        assertThat(
            Statuses.displayStatus(CapturePhase.IDLE, recording(server = "COMPLETED"), 0),
        ).isEqualTo(DisplayStatus.COMPLETED)
    }

    @Test fun failed_local_state_overrides() {
        assertThat(
            Statuses.displayStatus(CapturePhase.IDLE, recording(local = "FAILED"), 0),
        ).isEqualTo(DisplayStatus.FAILED)
    }

    @Test fun pending_chunks_show_uploading() {
        assertThat(
            Statuses.displayStatus(CapturePhase.IDLE, recording(server = "UNKNOWN"), 2),
        ).isEqualTo(DisplayStatus.UPLOADING)
    }

    @Test fun a_requested_retry_shows_retrying_instead_of_a_sticky_failure() {
        // The reset already cleared localState, but even a racing read must not look terminal.
        assertThat(
            Statuses.displayStatus(
                CapturePhase.IDLE,
                recording(local = "FAILED", retryRequestedAtEpochMs = 42L),
                0,
            ),
        ).isEqualTo(DisplayStatus.RETRYING)

        assertThat(
            Statuses.displayStatus(
                CapturePhase.IDLE,
                recording(local = "COMPLETING", server = "UPLOADING", retryRequestedAtEpochMs = 42L),
                1,
            ),
        ).isEqualTo(DisplayStatus.RETRYING)
    }

    @Test fun without_a_retry_marker_a_failure_stays_visible() {
        assertThat(
            Statuses.displayStatus(CapturePhase.IDLE, recording(local = "FAILED"), 0),
        ).isEqualTo(DisplayStatus.FAILED)
        assertThat(
            Statuses.displayStatus(CapturePhase.IDLE, recording(server = "COMPLETED"), 0),
        ).isEqualTo(DisplayStatus.COMPLETED)
    }

    @Test fun transcribing_and_refining() {
        assertThat(Statuses.displayStatus(CapturePhase.IDLE, recording(server = "TRANSCRIBING"), 0))
            .isEqualTo(DisplayStatus.TRANSCRIBING)
        assertThat(Statuses.displayStatus(CapturePhase.IDLE, recording(server = "REFINING"), 0))
            .isEqualTo(DisplayStatus.REFINING)
    }

    @Test fun elapsed_formatting() {
        assertThat(Statuses.formatElapsed(0)).isEqualTo("0:00.0")
        assertThat(Statuses.formatElapsed(65_400)).isEqualTo("1:05.4")
        assertThat(Statuses.formatElapsed(600_000)).isEqualTo("10:00.0")
    }
}
