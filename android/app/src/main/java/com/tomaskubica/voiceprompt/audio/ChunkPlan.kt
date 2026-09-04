package com.tomaskubica.voiceprompt.audio

/**
 * Pure chunk-boundary math for the exact 30.0 s window / 1.5 s overlap contract.
 *
 * Windows are defined in whole PCM samples at [SAMPLE_RATE]:
 *  - window  = 30.0 s -> 480_000 samples
 *  - overlap =  1.5 s ->  24_000 samples
 *  - advance = window - overlap = 456_000 samples (28.5 s)
 *
 * Chunk `i` (0-based) covers absolute samples `[i*advance, i*advance + window)`.
 * So chunk 0 = [0, 30s], chunk 1 = [28.5s, 58.5s], chunk 2 = [57.0s, 87.0s], ...
 *
 * This object holds no audio; it only decides boundaries so it is trivially unit-testable.
 */
object ChunkPlan {
    const val SAMPLE_RATE = 16_000
    const val BYTES_PER_SAMPLE = 2 // mono, signed PCM 16-bit

    const val WINDOW_SAMPLES: Long = 480_000L    // 30.0 s
    const val OVERLAP_SAMPLES: Long = 24_000L    //  1.5 s
    const val ADVANCE_SAMPLES: Long = WINDOW_SAMPLES - OVERLAP_SAMPLES // 456_000 (28.5 s)

    const val WINDOW_MS = 30_000
    const val OVERLAP_MS = 1_500

    /** Absolute start sample of chunk [index]. */
    fun chunkStartSample(index: Int): Long {
        require(index >= 0) { "index must be >= 0" }
        return index * ADVANCE_SAMPLES
    }

    /** A full [WINDOW_SAMPLES] chunk at [index] is ready once this many samples exist. */
    fun samplesNeededForChunk(index: Int): Long = chunkStartSample(index) + WINDOW_SAMPLES

    /** True when enough total samples have been captured to emit the full chunk at [index]. */
    fun isChunkReady(index: Int, totalSamples: Long): Boolean =
        totalSamples >= samplesNeededForChunk(index)

    /**
     * Decide the final (remainder) chunk to emit at stop.
     *
     * [nextIndex] is the index of the next not-yet-emitted chunk; [totalSamples] is everything
     * captured. Returns the remainder window, or `null` when the only remaining samples are the
     * retained overlap already contained in the previous chunk (so we must NOT emit them again).
     *
     * The previous full chunk `nextIndex-1` already covered up to
     * `chunkStartSample(nextIndex) + OVERLAP_SAMPLES`. Genuinely new audio therefore exists only
     * when the remainder is strictly longer than the overlap. For the very first chunk there is
     * no prior overlap, so any samples count as new.
     */
    fun finalChunkAtStop(nextIndex: Int, totalSamples: Long): FinalChunk? {
        val start = chunkStartSample(nextIndex)
        val remainder = totalSamples - start
        if (remainder <= 0) return null
        val hasPriorOverlap = nextIndex > 0
        if (hasPriorOverlap && remainder <= OVERLAP_SAMPLES) {
            // Only retained overlap remains — emitting would duplicate the previous chunk's tail.
            return null
        }
        return FinalChunk(index = nextIndex, startSample = start, lengthSamples = remainder)
    }

    /** Milliseconds represented by a sample count. */
    fun samplesToMs(samples: Long): Int = ((samples * 1000L) / SAMPLE_RATE).toInt()

    data class FinalChunk(val index: Int, val startSample: Long, val lengthSamples: Long)
}
