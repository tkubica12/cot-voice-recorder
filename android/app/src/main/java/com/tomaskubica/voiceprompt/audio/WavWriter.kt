package com.tomaskubica.voiceprompt.audio

import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Writes canonical 16 kHz mono signed 16-bit little-endian PCM WAV data.
 *
 * The 44-byte header is a standard RIFF/WAVE + fmt + data layout with all multi-byte fields
 * encoded little-endian, matching the upload contract (16 kHz, mono, 16-bit PCM).
 */
object WavWriter {
    const val HEADER_SIZE = 44

    private const val CHANNELS = 1
    private const val BITS_PER_SAMPLE = 16

    /** Build the 44-byte WAV header for a PCM payload of [dataSize] bytes at [sampleRate]. */
    fun header(dataSize: Int, sampleRate: Int = ChunkPlan.SAMPLE_RATE): ByteArray {
        val byteRate = sampleRate * CHANNELS * (BITS_PER_SAMPLE / 8)
        val blockAlign = CHANNELS * (BITS_PER_SAMPLE / 8)
        val buf = ByteBuffer.allocate(HEADER_SIZE).order(ByteOrder.LITTLE_ENDIAN)
        buf.put('R'.code.toByte()).put('I'.code.toByte()).put('F'.code.toByte()).put('F'.code.toByte())
        buf.putInt(36 + dataSize)              // ChunkSize = 4 + (8 + 16) + (8 + dataSize)
        buf.put('W'.code.toByte()).put('A'.code.toByte()).put('V'.code.toByte()).put('E'.code.toByte())
        // fmt subchunk
        buf.put('f'.code.toByte()).put('m'.code.toByte()).put('t'.code.toByte()).put(' '.code.toByte())
        buf.putInt(16)                         // Subchunk1Size for PCM
        buf.putShort(1)                        // AudioFormat = 1 (PCM)
        buf.putShort(CHANNELS.toShort())
        buf.putInt(sampleRate)
        buf.putInt(byteRate)
        buf.putShort(blockAlign.toShort())
        buf.putShort(BITS_PER_SAMPLE.toShort())
        // data subchunk
        buf.put('d'.code.toByte()).put('a'.code.toByte()).put('t'.code.toByte()).put('a'.code.toByte())
        buf.putInt(dataSize)
        return buf.array()
    }

    /** Return a full WAV file (header + PCM) as a byte array. */
    fun wavBytes(pcm: ByteArray, sampleRate: Int = ChunkPlan.SAMPLE_RATE): ByteArray {
        val header = header(pcm.size, sampleRate)
        return header + pcm
    }

    /** Stream a full WAV file (header + PCM) to [out]. */
    fun writeWav(out: OutputStream, pcm: ByteArray, sampleRate: Int = ChunkPlan.SAMPLE_RATE) {
        out.write(header(pcm.size, sampleRate))
        out.write(pcm)
    }
}
