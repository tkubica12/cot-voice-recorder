package com.tomaskubica.voiceprompt.service

import com.tomaskubica.voiceprompt.audio.PcmSource
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.work.UploadWorkScheduler
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Drives one capture session: reads PCM from a [PcmSource], windows it into chunks with
 * [StreamingChunker], and persists + enqueues each chunk. Capture and persistence are decoupled
 * through a channel so DB/file writes never stall the audio read loop.
 *
 * No audio content is ever logged.
 */
class RecordingController(
    private val repository: RecordingRepository,
    private val scheduler: UploadWorkScheduler,
    private val sourceFactory: () -> PcmSource,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    /**
     * Record until [shouldStop] returns true, then finalize. Returns the number of chunks emitted
     * (0 if no genuine audio was captured). Suspends until all captured chunks are persisted.
     */
    suspend fun record(
        clientId: String,
        startEpochMs: Long,
        shouldStop: () -> Boolean,
    ): Int = coroutineScope {
        scheduler.startChain(clientId)

        val channel = Channel<StreamingChunker.RawChunk>(Channel.UNLIMITED)
        val chunker = StreamingChunker { channel.trySend(it) }

        val consumer = launch(Dispatchers.IO) {
            for (raw in channel) {
                repository.persistChunk(clientId, raw, startEpochMs)
                scheduler.enqueueChunk(clientId, raw.index)
                RecorderState.onChunkCaptured(raw.index + 1)
            }
        }

        withContext(Dispatchers.IO) {
            val source = sourceFactory()
            source.start()
            try {
                val buf = ByteArray(source.bufferSize)
                while (!shouldStop()) {
                    val n = source.read(buf)
                    if (n > 0) {
                        val aligned = n - (n % 2)
                        if (aligned > 0) chunker.append(buf, aligned)
                        RecorderState.onElapsed(clock() - startEpochMs)
                    } else if (n < 0) {
                        break
                    }
                }
            } finally {
                source.stop()
            }
            chunker.finalize()
            channel.close()
        }

        consumer.join()
        chunker.emittedCount()
    }
}
