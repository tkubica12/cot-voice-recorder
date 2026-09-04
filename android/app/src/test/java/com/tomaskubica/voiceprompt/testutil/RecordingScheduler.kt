package com.tomaskubica.voiceprompt.testutil

import com.tomaskubica.voiceprompt.work.UploadWorkScheduler

/** Records scheduling calls so capture logic can be asserted without WorkManager. */
class RecordingScheduler : UploadWorkScheduler {
    val started = mutableListOf<String>()
    val chunks = mutableListOf<Pair<String, Int>>()
    val completes = mutableListOf<String>()
    val retries = mutableListOf<Triple<String, List<Int>, Boolean>>()

    /** When set, every scheduling call throws it (simulates a WorkManager failure). */
    var failWith: Throwable? = null

    override fun startChain(clientId: String) {
        failWith?.let { throw it }
        started += clientId
    }

    override fun enqueueChunk(clientId: String, index: Int) {
        failWith?.let { throw it }
        chunks += clientId to index
    }

    override fun enqueueComplete(clientId: String) {
        failWith?.let { throw it }
        completes += clientId
    }

    override fun retry(clientId: String, pendingIndices: List<Int>, hasComplete: Boolean) {
        failWith?.let { throw it }
        retries += Triple(clientId, pendingIndices, hasComplete)
    }
}
