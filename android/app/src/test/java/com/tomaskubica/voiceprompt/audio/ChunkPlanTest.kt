package com.tomaskubica.voiceprompt.audio

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class ChunkPlanTest {

    @Test fun constants_match_30s_window_and_1_5s_overlap() {
        assertThat(ChunkPlan.WINDOW_SAMPLES).isEqualTo(480_000L)   // 30.0 s @ 16 kHz
        assertThat(ChunkPlan.OVERLAP_SAMPLES).isEqualTo(24_000L)   //  1.5 s
        assertThat(ChunkPlan.ADVANCE_SAMPLES).isEqualTo(456_000L)  // 28.5 s stride
    }

    @Test fun chunk_start_advances_by_stride() {
        assertThat(ChunkPlan.chunkStartSample(0)).isEqualTo(0L)
        assertThat(ChunkPlan.chunkStartSample(1)).isEqualTo(456_000L)
        assertThat(ChunkPlan.chunkStartSample(2)).isEqualTo(912_000L)
    }

    @Test fun chunk_ready_at_exact_window_boundary() {
        assertThat(ChunkPlan.isChunkReady(0, 479_999L)).isFalse()
        assertThat(ChunkPlan.isChunkReady(0, 480_000L)).isTrue()
        assertThat(ChunkPlan.isChunkReady(1, 935_999L)).isFalse()
        assertThat(ChunkPlan.isChunkReady(1, 936_000L)).isTrue()
    }

    @Test fun final_chunk_null_when_no_samples() {
        assertThat(ChunkPlan.finalChunkAtStop(0, 0)).isNull()
    }

    @Test fun final_chunk_emits_first_remainder_with_no_prior_overlap() {
        val fc = ChunkPlan.finalChunkAtStop(0, 10_000L)
        assertThat(fc).isNotNull()
        assertThat(fc!!.index).isEqualTo(0)
        assertThat(fc.startSample).isEqualTo(0L)
        assertThat(fc.lengthSamples).isEqualTo(10_000L)
    }

    @Test fun final_chunk_null_when_only_retained_overlap_remains() {
        // Exactly one overlap of new audio after chunk 0 -> already contained in chunk 0.
        assertThat(ChunkPlan.finalChunkAtStop(1, 456_000L + 24_000L)).isNull()
        // And nothing beyond the window start.
        assertThat(ChunkPlan.finalChunkAtStop(1, 456_000L)).isNull()
    }

    @Test fun final_chunk_emits_when_genuinely_new_audio_beyond_overlap() {
        val fc = ChunkPlan.finalChunkAtStop(1, 456_000L + 24_001L)
        assertThat(fc).isNotNull()
        assertThat(fc!!.index).isEqualTo(1)
        assertThat(fc.startSample).isEqualTo(456_000L)
        assertThat(fc.lengthSamples).isEqualTo(24_001L)
    }

    @Test fun samples_to_ms() {
        assertThat(ChunkPlan.samplesToMs(480_000L)).isEqualTo(30_000)
        assertThat(ChunkPlan.samplesToMs(24_000L)).isEqualTo(1_500)
    }
}
