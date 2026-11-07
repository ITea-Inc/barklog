package app.barklog.ui

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaPlayer
import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Откуда брать звук эпизода. */
sealed interface ClipSource {
    /** Исходный файл (выбран через системный диалог). */
    data class File(val uri: Uri) : ClipSource

    /** Фрагменты, сохранённые при записи в приложении: PCM 16 бит, 16 кГц. */
    class Pcm(val load: suspend (startMs: Long, endMs: Long) -> ByteArray) : ClipSource
}

/**
 * Проигрывает эпизоды: от начала (с запасом 0,5 с) до конца.
 * Для файла плеер готовится один раз и переиспользуется между эпизодами.
 */
class EpisodePlayer(private val context: Context, private val scope: CoroutineScope) {

    /** Индекс играющего эпизода. */
    var playing by mutableStateOf<Int?>(null)
        private set

    /** Доля проигранного фрагмента, 0..1. */
    var progress by mutableStateOf(0f)
        private set

    var error by mutableStateOf<String?>(null)
        private set

    private var player: MediaPlayer? = null
    private var preparedUri: Uri? = null
    private var job: Job? = null

    fun toggle(index: Int, source: ClipSource, startMs: Long, endMs: Long) {
        if (playing == index) {
            stop()
            return
        }
        stop()
        val from = (startMs - PAD_MS).coerceAtLeast(0)
        val to = endMs + PAD_MS
        when (source) {
            is ClipSource.File -> playFile(index, source.uri, from, to)
            is ClipSource.Pcm -> playPcm(index, source, from, to)
        }
    }

    private fun playPcm(index: Int, source: ClipSource.Pcm, from: Long, to: Long) {
        job = scope.launch {
            var track: AudioTrack? = null
            try {
                val pcm = source.load(from, to)
                val frames = pcm.size / 2
                if (frames == 0) return@launch
                track = AudioTrack.Builder()
                    .setAudioAttributes(attributes())
                    .setAudioFormat(
                        AudioFormat.Builder()
                            .setSampleRate(PCM_RATE)
                            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                            .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                            .build(),
                    )
                    .setTransferMode(AudioTrack.MODE_STATIC)
                    .setBufferSizeInBytes(pcm.size)
                    .build()
                track.write(pcm, 0, pcm.size)
                track.play()
                playing = index
                error = null
                while (isActive && track.playbackHeadPosition < frames) {
                    progress = track.playbackHeadPosition.toFloat() / frames
                    delay(50)
                }
            } catch (e: Exception) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                error = "Фрагмент не найден — возможно, данные записи удалены"
            } finally {
                track?.run {
                    runCatching { stop() }
                    release()
                }
                if (playing == index) {
                    playing = null
                    progress = 0f
                }
            }
        }
    }

    private fun playFile(index: Int, uri: Uri, from: Long, to: Long) {
        job = scope.launch {
            try {
                val mp = prepared(uri)
                mp.seekTo(from, MediaPlayer.SEEK_CLOSEST)
                mp.start()
                playing = index
                error = null
                while (isActive && mp.isPlaying && mp.currentPosition < to) {
                    progress = ((mp.currentPosition - from).toFloat() / (to - from)).coerceIn(0f, 1f)
                    delay(50)
                }
            } catch (e: Exception) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                error = "Исходный файл недоступен — возможно, его удалили или переместили"
                releasePlayer()
            } finally {
                // Если уже включили другой эпизод, плеер принадлежит ему — не трогаем.
                if (playing == index) {
                    player?.takeIf { it.isPlaying }?.pause()
                    playing = null
                    progress = 0f
                }
            }
        }
    }

    fun stop() {
        job?.cancel()
        job = null
        player?.takeIf { it.isPlaying }?.pause()
        playing = null
        progress = 0f
    }

    fun release() {
        stop()
        releasePlayer()
    }

    private suspend fun prepared(uri: Uri): MediaPlayer {
        player?.let { if (preparedUri == uri) return it }
        releasePlayer()
        val mp = withContext(Dispatchers.IO) {
            MediaPlayer().apply {
                setAudioAttributes(attributes())
                setDataSource(context, uri)
                prepare()
            }
        }
        player = mp
        preparedUri = uri
        return mp
    }

    private fun attributes() = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_UNKNOWN)
        .build()

    private fun releasePlayer() {
        player?.release()
        player = null
        preparedUri = null
    }

    private companion object {
        const val PAD_MS = 500L
        const val PCM_RATE = 16_000
    }
}

@Composable
fun rememberEpisodePlayer(): EpisodePlayer {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val player = remember { EpisodePlayer(context.applicationContext, scope) }
    DisposableEffect(player) { onDispose { player.release() } }
    return player
}
