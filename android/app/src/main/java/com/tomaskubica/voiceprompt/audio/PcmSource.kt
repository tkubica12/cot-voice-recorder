package com.tomaskubica.voiceprompt.audio

/**
 * Abstraction over a PCM16 microphone source so capture logic can be tested without a real mic.
 * [read] fills [buffer] and returns the number of bytes read (>0), or a negative value on error.
 */
interface PcmSource {
    /** Start capture. Throws if the source cannot be opened. */
    fun start()

    /** Blocking read of up to [buffer].size bytes. Returns bytes read, or < 0 on error. */
    fun read(buffer: ByteArray): Int

    /** Stop and release. */
    fun stop()

    /** Preferred read buffer size in bytes. */
    val bufferSize: Int
}
