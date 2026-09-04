package com.tomaskubica.voiceprompt.audio

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder

/**
 * [PcmSource] backed by [AudioRecord] capturing 16 kHz mono signed PCM 16-bit.
 *
 * The read buffer is sized to a fraction of a chunk window so the capture loop stays responsive.
 */
class AudioRecordSource : PcmSource {

    private var record: AudioRecord? = null

    private val minBuf = AudioRecord.getMinBufferSize(
        ChunkPlan.SAMPLE_RATE,
        AudioFormat.CHANNEL_IN_MONO,
        AudioFormat.ENCODING_PCM_16BIT,
    ).coerceAtLeast(ChunkPlan.SAMPLE_RATE * ChunkPlan.BYTES_PER_SAMPLE / 5) // >= ~200 ms

    override val bufferSize: Int = minBuf

    @SuppressLint("MissingPermission") // callers verify RECORD_AUDIO before start()
    override fun start() {
        val rec = AudioRecord(
            MediaRecorder.AudioSource.VOICE_RECOGNITION,
            ChunkPlan.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
            (minBuf * 4).coerceAtLeast(minBuf),
        )
        check(rec.state == AudioRecord.STATE_INITIALIZED) { "AudioRecord failed to initialize" }
        rec.startRecording()
        record = rec
    }

    override fun read(buffer: ByteArray): Int {
        val rec = record ?: return -1
        return rec.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)
    }

    override fun stop() {
        record?.run {
            runCatching { if (recordingState == AudioRecord.RECORDSTATE_RECORDING) stop() }
            runCatching { release() }
        }
        record = null
    }
}
