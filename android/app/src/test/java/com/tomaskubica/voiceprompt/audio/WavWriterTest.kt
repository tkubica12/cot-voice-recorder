package com.tomaskubica.voiceprompt.audio

import com.google.common.truth.Truth.assertThat
import java.nio.ByteBuffer
import java.nio.ByteOrder
import org.junit.Test

class WavWriterTest {

    private fun le(header: ByteArray, offset: Int, len: Int): Long {
        val bb = ByteBuffer.wrap(header, offset, len).order(ByteOrder.LITTLE_ENDIAN)
        return when (len) {
            2 -> bb.short.toLong() and 0xFFFF
            4 -> bb.int.toLong() and 0xFFFFFFFFL
            else -> error("len")
        }
    }

    @Test fun header_is_44_bytes() {
        assertThat(WavWriter.header(0).size).isEqualTo(44)
    }

    @Test fun riff_and_wave_magic() {
        val h = WavWriter.header(1000)
        assertThat(String(h.copyOfRange(0, 4))).isEqualTo("RIFF")
        assertThat(String(h.copyOfRange(8, 12))).isEqualTo("WAVE")
        assertThat(String(h.copyOfRange(12, 16))).isEqualTo("fmt ")
        assertThat(String(h.copyOfRange(36, 40))).isEqualTo("data")
    }

    @Test fun fmt_fields_are_16k_mono_pcm16_little_endian() {
        val dataSize = 960_000
        val h = WavWriter.header(dataSize)
        assertThat(le(h, 4, 4)).isEqualTo((36 + dataSize).toLong())  // RIFF chunk size
        assertThat(le(h, 16, 4)).isEqualTo(16L)                      // fmt subchunk size (PCM)
        assertThat(le(h, 20, 2)).isEqualTo(1L)                       // audio format = PCM
        assertThat(le(h, 22, 2)).isEqualTo(1L)                       // channels = mono
        assertThat(le(h, 24, 4)).isEqualTo(16_000L)                  // sample rate
        assertThat(le(h, 28, 4)).isEqualTo(32_000L)                  // byte rate = 16000*1*2
        assertThat(le(h, 32, 2)).isEqualTo(2L)                       // block align
        assertThat(le(h, 34, 2)).isEqualTo(16L)                      // bits per sample
        assertThat(le(h, 40, 4)).isEqualTo(dataSize.toLong())        // data subchunk size
    }

    @Test fun wav_bytes_is_header_plus_payload() {
        val pcm = ByteArray(200) { it.toByte() }
        val wav = WavWriter.wavBytes(pcm)
        assertThat(wav.size).isEqualTo(44 + pcm.size)
        assertThat(wav.copyOfRange(44, wav.size)).isEqualTo(pcm)
    }
}
