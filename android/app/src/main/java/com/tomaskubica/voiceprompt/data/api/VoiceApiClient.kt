package com.tomaskubica.voiceprompt.data.api

import com.squareup.moshi.Moshi
import okhttp3.Headers
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.IOException

/**
 * Thin OkHttp client implementing the Voice Recorder API exactly per the OpenAPI contract:
 * bearer auth, JSON bodies, raw `audio/wav` chunk upload with `Content-Digest`, and RFC 9457
 * problem parsing. Never logs audio bytes.
 */
class VoiceApiClient(
    private val baseUrlProvider: () -> String,
    private val http: OkHttpClient = defaultClient(),
    moshi: Moshi = defaultMoshi(),
) {
    private val jsonMedia = "application/json".toMediaType()
    private val wavMedia = "audio/wav".toMediaType()

    private val createReqAdapter = moshi.adapter(CreateRecordingRequestDto::class.java)
    private val completeReqAdapter = moshi.adapter(CompleteRecordingRequestDto::class.java)
    private val recordingAdapter = moshi.adapter(RecordingDto::class.java)
    private val chunkAdapter = moshi.adapter(ChunkAcceptedDto::class.java)
    private val transcriptAdapter = moshi.adapter(TranscriptDto::class.java)
    private val transcriptPageAdapter = moshi.adapter(TranscriptListPageDto::class.java)
    private val problemAdapter = moshi.adapter(ProblemDto::class.java)

    private fun base(): String = baseUrlProvider().trimEnd('/')

    // ------------------------------------------------------------------ health
    /** Unauthenticated warmup probe. Returns true only on a 200 ready response. */
    fun healthReady(): Boolean = try {
        val req = Request.Builder().url("${base()}/health/ready").get().build()
        http.newCall(req).execute().use { it.isSuccessful }
    } catch (_: Exception) {
        false
    }

    // -------------------------------------------------------------- recordings
    fun createRecording(token: String, body: CreateRecordingRequestDto): ApiResult<RecordingDto> {
        val req = authedJson(token, "${base()}/v1/recordings")
            .post(createReqAdapter.toJson(body).toRequestBody(jsonMedia))
            .build()
        return execute(req, recordingAdapter)
    }

    fun getRecording(token: String, recordingId: String): ApiResult<RecordingDto> {
        val req = authedJson(token, "${base()}/v1/recordings/$recordingId").get().build()
        return execute(req, recordingAdapter)
    }

    fun uploadChunk(
        token: String,
        recordingId: String,
        index: Int,
        wavBytes: ByteArray,
        contentDigest: String,
        durationMs: Int?,
        overlapMs: Int?,
        startedAtIso: String?,
    ): ApiResult<ChunkAcceptedDto> {
        val builder = authedBase(token, "${base()}/v1/recordings/$recordingId/chunks/$index")
            .put(wavBytes.toRequestBody(wavMedia))
            .header("Content-Digest", contentDigest)
        durationMs?.let { builder.header("X-Chunk-Duration-Ms", it.toString()) }
        overlapMs?.let { builder.header("X-Chunk-Overlap-Ms", it.toString()) }
        startedAtIso?.let { builder.header("X-Chunk-Started-At", it) }
        return execute(builder.build(), chunkAdapter)
    }

    fun completeRecording(
        token: String,
        recordingId: String,
        body: CompleteRecordingRequestDto,
    ): ApiResult<RecordingDto> {
        val req = authedJson(token, "${base()}/v1/recordings/$recordingId/complete")
            .post(completeReqAdapter.toJson(body).toRequestBody(jsonMedia))
            .build()
        return execute(req, recordingAdapter)
    }

    // -------------------------------------------------------------- transcripts
    fun listTranscripts(token: String, cursor: String?, limit: Int?): ApiResult<TranscriptListPageDto> {
        val url = "${base()}/v1/transcripts".toHttpUrl().newBuilder().apply {
            cursor?.let { addQueryParameter("cursor", it) }
            limit?.let { addQueryParameter("limit", it.toString()) }
        }.build()
        val req = authedBase(token, url.toString()).get().build()
        return execute(req, transcriptPageAdapter)
    }

    fun getTranscript(token: String, transcriptId: String): ApiResult<TranscriptDto> {
        val req = authedJson(token, "${base()}/v1/transcripts/$transcriptId").get().build()
        return execute(req, transcriptAdapter)
    }

    // ------------------------------------------------------------------ helpers
    private fun authedBase(token: String, url: String): Request.Builder =
        Request.Builder().url(url).header("Authorization", "Bearer $token")

    private fun authedJson(token: String, url: String): Request.Builder =
        authedBase(token, url).header("Accept", "application/json")

    private fun <T> execute(
        req: Request,
        adapter: com.squareup.moshi.JsonAdapter<T>,
    ): ApiResult<T> {
        return try {
            http.newCall(req).execute().use { resp ->
                val bodyStr = resp.body?.string()
                if (resp.isSuccessful) {
                    val parsed = bodyStr?.let { adapter.fromJson(it) }
                    if (parsed != null) {
                        ApiResult.Success(resp.code, parsed)
                    } else {
                        ApiResult.Failure(resp.code, null)
                    }
                } else {
                    val problem = bodyStr?.let { runCatching { problemAdapter.fromJson(it) }.getOrNull() }
                    ApiResult.Failure(resp.code, problem)
                }
            }
        } catch (e: IOException) {
            ApiResult.NetworkError(e)
        } catch (e: Exception) {
            ApiResult.NetworkError(e)
        }
    }

    companion object {
        fun defaultMoshi(): Moshi = Moshi.Builder().build()

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(java.time.Duration.ofSeconds(20))
            .readTimeout(java.time.Duration.ofSeconds(60))
            .writeTimeout(java.time.Duration.ofSeconds(60))
            .retryOnConnectionFailure(true)
            .build()
    }
}
