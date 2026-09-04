package com.tomaskubica.voiceprompt.data

import java.io.File

/**
 * Stores per-chunk WAV files under the app's private files dir:
 * `<filesDir>/chunks/<clientRecordingId>/<index>.wav`.
 *
 * Files are written before upload and deleted only after the backend acknowledges the chunk.
 */
class ChunkFileStore(private val baseDir: File) {

    private fun recordingDir(clientId: String): File =
        File(baseDir, "chunks/$clientId").apply { mkdirs() }

    fun fileFor(clientId: String, index: Int): File =
        File(recordingDir(clientId), "$index.wav")

    fun write(clientId: String, index: Int, wavBytes: ByteArray): File {
        val f = fileFor(clientId, index)
        f.outputStream().use { it.write(wavBytes) }
        return f
    }

    fun delete(path: String?): Boolean {
        if (path == null) return false
        return runCatching { File(path).delete() }.getOrDefault(false)
    }

    fun deleteRecording(clientId: String) {
        runCatching { File(baseDir, "chunks/$clientId").deleteRecursively() }
    }
}
