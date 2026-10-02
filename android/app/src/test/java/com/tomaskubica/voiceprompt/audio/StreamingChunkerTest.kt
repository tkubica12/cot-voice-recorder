package com.tomaskubica.voiceprompt.audio

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class StreamingChunkerTest {

    private fun silence(samples: Int) = ByteArray(samples * 2)

    private fun collect(feed: (StreamingChunker) -> Unit): List<StreamingChunker.RawChunk> {
        val out = mutableListOf<StreamingChunker.RawChunk>()
        val chunker = StreamingChunker { out.add(it) }
        feed(chunker)
        return out
    }

    @Test fun exactly_30s_yields_single_chunk_no_remainder() {
        val chunks = collect { c ->
            c.append(silence(480_000))
            c.finalize()
        }
        assertThat(chunks).hasSize(1)
        assertThat(chunks[0].index).isEqualTo(0)
        assertThat(chunks[0].lengthSamples).isEqualTo(480_000L)
        assertThat(chunks[0].pcm.size).isEqualTo(480_000 * 2)
        assertThat(chunks[0].overlapMs).isEqualTo(0)
        assertThat(chunks[0].durationMs).isEqualTo(30_000)
    }

    @Test fun two_full_windows_at_58_5s_no_remainder() {
        val chunks = collect { c ->
            c.append(silence(936_000)) // 58.5 s
            c.finalize()
        }
        assertThat(chunks).hasSize(2)
        assertThat(chunks.map { it.index }).containsExactly(0, 1).inOrder()
        assertThat(chunks[0].startSample).isEqualTo(0L)
        assertThat(chunks[1].startSample).isEqualTo(456_000L)
        assertThat(chunks[1].overlapMs).isEqualTo(1_500)
    }

    @Test fun remainder_emitted_only_when_beyond_overlap() {
        val chunks = collect { c ->
            c.append(silence(937_000)) // 58.5 s + a bit
            c.finalize()
        }
        assertThat(chunks).hasSize(3)
        val last = chunks[2]
        assertThat(last.index).isEqualTo(2)
        assertThat(last.startSample).isEqualTo(912_000L)
        assertThat(last.lengthSamples).isEqualTo(25_000L)
        assertThat(last.overlapMs).isEqualTo(1_500)
    }

    @Test fun overlap_only_remainder_is_not_emitted() {
        // 456000 + 24000 = 480000 samples total: chunk 0 emitted, remainder == overlap -> no emit.
        val chunks = collect { c ->
            c.append(silence(480_000))
            c.append(silence(0))
            c.finalize()
        }
        assertThat(chunks).hasSize(1)
    }

    @Test fun short_recording_under_one_window_emits_single_remainder() {
        val chunks = collect { c ->
            c.append(silence(16_000)) // 1 s
            c.finalize()
        }
        assertThat(chunks).hasSize(1)
        assertThat(chunks[0].index).isEqualTo(0)
        assertThat(chunks[0].lengthSamples).isEqualTo(16_000L)
        assertThat(chunks[0].overlapMs).isEqualTo(0)
    }

    @Test fun no_audio_emits_nothing() {
        val chunks = collect { c -> c.finalize() }
        assertThat(chunks).isEmpty()
    }

    @Test fun incremental_appends_produce_same_boundaries() {
        val chunks = collect { c ->
            repeat(937_000 / 1_000) { c.append(silence(1_000)) }
            c.finalize()
        }
        assertThat(chunks.map { it.index }).containsExactly(0, 1, 2).inOrder()
    }

    @Test fun contiguous_ten_second_segments_cover_every_sample_exactly_once() {
        val original = ByteArray(320_000 * 3 + 2_002) { (it % 251).toByte() }
        val chunks = mutableListOf<StreamingChunker.RawChunk>()
        val chunker = StreamingChunker(160_000, 0) { chunks.add(it) }
        var position = 0
        while (position < original.size) {
            val size = minOf(1_282, original.size - position)
            chunker.append(original.copyOfRange(position, position + size))
            position += size
        }
        chunker.finalize()
        assertThat(chunks).hasSize(4)
        assertThat(chunks.map { it.startSample }).containsExactly(0L, 160_000L, 320_000L, 480_000L).inOrder()
        assertThat(chunks.all { it.overlapMs == 0 && !it.hasPriorOverlap }).isTrue()
        assertThat(chunks.flatMap { it.pcm.asIterable() }.toByteArray()).isEqualTo(original)
        assertThat(chunks.last().lengthSamples).isEqualTo(1_001L)
    }

    @Test fun exact_contiguous_window_does_not_emit_an_empty_tail() {
        val chunks = mutableListOf<StreamingChunker.RawChunk>()
        val chunker = StreamingChunker(160_000, 0) { chunks.add(it) }
        chunker.append(silence(320_000))
        chunker.finalize()
        assertThat(chunks).hasSize(2)
    }
}
