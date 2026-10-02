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
import java.io.IOException

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
        onCaptureStarted: () -> Unit = {},
    ): Int = coroutineScope {
        repository.withUploadScheduling { scheduler.startChain(clientId) }

        val channel = Channel<StreamingChunker.RawChunk>(4)
        val recording = repository.getRecording(clientId) ?: throw IOException("Recording metadata is missing")
        val contiguous = recording.transcriptionMode == "streaming" && recording.audioLayout == "contiguous"
        val chunker = StreamingChunker(
            onChunk = {
                if (channel.trySend(it).isFailure) throw IOException("Audio persistence cannot keep up")
            },
            windowSamples = if (contiguous) 160_000 else 480_000,
            overlapSamples = if (contiguous) 0 else 24_000,
        )

        val consumer = launch(Dispatchers.IO) {
            for (raw in channel) {
                repository.withUploadScheduling {
                    repository.persistChunk(clientId, raw, startEpochMs)
                    scheduler.enqueueChunk(clientId, raw.index)
                }
                RecorderState.onChunkCaptured(raw.index + 1)
            }
        }

        var captureFailure: Throwable? = null
        try {
            withContext(Dispatchers.IO) {
                val source = sourceFactory()
                source.start()
                try {
                    onCaptureStarted()
                    val buf = ByteArray(source.bufferSize)
                    while (!shouldStop()) {
                        val n = source.read(buf)
                        if (n > 0) {
                            if (n % 2 != 0) throw IOException("Microphone returned an incomplete PCM sample")
                            chunker.append(buf, n)
                            RecorderState.onElapsed(clock() - startEpochMs)
                        } else if (n < 0) {
                            break
                        }
                    }
                } finally {
                    try { source.stop() } finally { chunker.finalize() }
                }
            }
        } catch (error: Throwable) {
            captureFailure = error
        } finally {
            channel.close()
        }

        consumer.join()
        captureFailure?.let { throw it }
        chunker.emittedCount()
    }
}
