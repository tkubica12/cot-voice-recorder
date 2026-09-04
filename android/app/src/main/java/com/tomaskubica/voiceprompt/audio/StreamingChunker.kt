package com.tomaskubica.voiceprompt.audio

/**
 * Streaming windowing of a mono PCM16 sample stream into [ChunkPlan] windows.
 *
 * Feed captured little-endian PCM bytes via [append]; full 30 s windows are emitted through
 * [onChunk] as soon as enough audio exists. Call [finalize] once at stop to emit the genuinely
 * new remainder (never a chunk that only repeats the retained 1.5 s overlap).
 *
 * The class is pure (no Android APIs) so the boundary/overlap/remainder behaviour is unit
 * testable. It retains at most one window of audio in memory.
 */
class StreamingChunker(
    private val onChunk: (RawChunk) -> Unit,
) {
    private val windowBytes = (ChunkPlan.WINDOW_SAMPLES * ChunkPlan.BYTES_PER_SAMPLE).toInt()
    private val advanceBytes = (ChunkPlan.ADVANCE_SAMPLES * ChunkPlan.BYTES_PER_SAMPLE).toInt()

    // Buffer holds bytes for samples from `retainedStartSample` onward.
    private var buf = ByteArray(windowBytes + advanceBytes)
    private var size = 0
    private var retainedStartSample = 0L

    var totalSamples = 0L
        private set
    var nextIndex = 0
        private set
    private var finalized = false

    /** Append [length] bytes of PCM16 little-endian audio from [data]. */
    fun append(data: ByteArray, length: Int = data.size) {
        check(!finalized) { "append after finalize" }
        require(length % ChunkPlan.BYTES_PER_SAMPLE == 0) { "PCM length must be sample-aligned" }
        ensureCapacity(size + length)
        System.arraycopy(data, 0, buf, size, length)
        size += length
        totalSamples += (length / ChunkPlan.BYTES_PER_SAMPLE).toLong()
        drainReadyWindows()
    }

    private fun drainReadyWindows() {
        while (size >= windowBytes) {
            val pcm = buf.copyOfRange(0, windowBytes)
            val start = retainedStartSample
            onChunk(
                RawChunk(
                    index = nextIndex,
                    startSample = start,
                    lengthSamples = ChunkPlan.WINDOW_SAMPLES,
                    pcm = pcm,
                    hasPriorOverlap = nextIndex > 0,
                ),
            )
            // Advance the window by the non-overlapping stride.
            System.arraycopy(buf, advanceBytes, buf, 0, size - advanceBytes)
            size -= advanceBytes
            retainedStartSample += ChunkPlan.ADVANCE_SAMPLES
            nextIndex += 1
        }
    }

    /** Emit the final remainder chunk (if any genuinely new audio remains) and stop. */
    fun finalize() {
        if (finalized) return
        finalized = true
        val remainderSamples = totalSamples - retainedStartSample
        val plan = ChunkPlan.finalChunkAtStop(nextIndex, totalSamples) ?: return
        val remainderBytes = (remainderSamples * ChunkPlan.BYTES_PER_SAMPLE).toInt()
        val pcm = buf.copyOfRange(0, remainderBytes)
        onChunk(
            RawChunk(
                index = plan.index,
                startSample = plan.startSample,
                lengthSamples = plan.lengthSamples,
                pcm = pcm,
                hasPriorOverlap = plan.index > 0,
            ),
        )
        nextIndex += 1
    }

    /** Number of chunks emitted so far. */
    fun emittedCount(): Int = nextIndex

    private fun ensureCapacity(needed: Int) {
        if (needed <= buf.size) return
        var newSize = buf.size
        while (newSize < needed) newSize += advanceBytes
        buf = buf.copyOf(newSize)
    }

    data class RawChunk(
        val index: Int,
        val startSample: Long,
        val lengthSamples: Long,
        val pcm: ByteArray,
        val hasPriorOverlap: Boolean,
    ) {
        val durationMs: Int get() = ChunkPlan.samplesToMs(lengthSamples)
        val overlapMs: Int get() = if (hasPriorOverlap) ChunkPlan.OVERLAP_MS else 0
        val startOffsetMs: Int get() = ChunkPlan.samplesToMs(startSample)

        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is RawChunk) return false
            return index == other.index &&
                startSample == other.startSample &&
                lengthSamples == other.lengthSamples &&
                pcm.contentEquals(other.pcm)
        }

        override fun hashCode(): Int = index
    }
}
