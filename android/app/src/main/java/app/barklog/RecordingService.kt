package app.barklog

import android.annotation.SuppressLint
import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.net.Uri
import android.os.Build
import android.os.IBinder
import android.util.Log
import android.os.PowerManager
import android.os.SystemClock
import android.provider.OpenableColumns
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import app.barklog.ui.Fmt
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.barklog_ffi.BarkLogException
import uniffi.barklog_ffi.LiveSession
import uniffi.barklog_ffi.PcmFileReader
import java.io.File
import java.util.TimeZone
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Режим «Я ушёл»: пишет звук с микрофона и отдаёт его ядру на Rust порциями по 100 мс.
 * Анализ идёт на лету, звук целиком не сохраняется — только фрагменты с собакой.
 */
class RecordingService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val stopRequested = AtomicBoolean(false)
    @Volatile private var startedAtMs = System.currentTimeMillis()
    private var wakeLock: PowerManager.WakeLock? = null
    private val app get() = application as BarkLogApp

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> stopRequested.set(true)
            ACTION_START -> if (!app.live.isActive) start(intent.getStringExtra(EXTRA_SIMULATE)?.let(Uri::parse))
        }
        return START_NOT_STICKY
    }

    private fun start(simulate: Uri?) {
        stopRequested.set(false)
        startedAtMs = System.currentTimeMillis()
        app.live.set(LiveState.Starting)
        // Запись из файла (отладка) микрофон не трогает — и разрешение на него не нужно.
        val type = when {
            simulate == null -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            Build.VERSION.SDK_INT >= 35 -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROCESSING
            else -> ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
        }
        try {
            // ServiceCompat отбрасывает тип mediaProcessing — на Android 10+ вызываем напрямую.
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION_ID, notification(null), type)
            } else {
                startForeground(NOTIFICATION_ID, notification(null))
            }
        } catch (e: RuntimeException) {
            // Нет доступа к микрофону или система не разрешила фоновую службу.
            Log.w(TAG, "startForeground failed", e)
            app.live.set(LiveState.Failed("Не удалось начать запись: нет доступа к микрофону"))
            stopSelf()
            return
        }
        wakeLock = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "barklog:recording")
            .apply { acquire(MAX_RECORDING_MS) }

        scope.launch {
            try {
                val core = app.core()
                val offsetMin = TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60_000
                val clipsRoot = File(filesDir, "clips").apply { mkdirs() }
                val session = core.startLive(SAMPLE_RATE.toUInt(), offsetMin, clipsRoot.absolutePath)
                app.live.set(LiveState.Recording(null))
                launch { publishSnapshots(session) }
                if (simulate != null) feedFile(session, simulate) else feedMicrophone(session)
                app.live.set(LiveState.Saving)
                // Последний снимок и отметка «запись закончена» для второго телефона.
                val finalJson = runCatching { session.snapshotJson() }.getOrNull()
                val finalSnap = runCatching { session.snapshot() }.getOrNull()
                app.remote.publish(recording = false, startedAtMs, finalJson, finalSnap)
                val id = session.finish()
                app.live.set(LiveState.Finished(id))
            } catch (e: BarkLogException) {
                app.live.set(LiveState.Failed(e.cleanMessage()))
            } catch (e: SecurityException) {
                app.live.set(LiveState.Failed("Нет доступа к микрофону"))
            } catch (e: IllegalStateException) {
                app.live.set(LiveState.Failed(e.message ?: "Не удалось начать запись"))
            } finally {
                shutdown()
            }
        }
    }

    @SuppressLint("MissingPermission") // разрешение запрашивает экран перед запуском сервиса
    private fun feedMicrophone(session: LiveSession) {
        val minBuf = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        // Запас на ~4 с: пока сеть считает пакет, звук копится в буфере AudioRecord.
        val bufferBytes = maxOf(minBuf, SAMPLE_RATE * 2 * 4)
        val record = AudioRecord(audioSource(), SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, bufferBytes)
        check(record.state == AudioRecord.STATE_INITIALIZED) { "Микрофон занят другим приложением" }
        val chunk = ByteArray(CHUNK_BYTES)
        try {
            record.startRecording()
            while (!stopRequested.get()) {
                val n = record.read(chunk, 0, chunk.size)
                if (n > 0) session.pushPcm16(chunk.copyOf(n))
                // n < 0 — микрофон временно недоступен (например, звонок): ждём и пробуем снова.
                if (n < 0) SystemClock.sleep(200)
            }
        } finally {
            record.stop()
            record.release()
        }
    }

    /** Источник без системного шумоподавления и АРУ, если устройство его поддерживает. */
    private fun audioSource(): Int {
        val unprocessed = getSystemService(AudioManager::class.java)
            .getProperty(AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED)
        return if (unprocessed == "true") MediaRecorder.AudioSource.UNPROCESSED else MediaRecorder.AudioSource.MIC
    }

    /** Отладка: «проигрывает» файл вместо микрофона, в 20 раз быстрее реального времени. */
    private fun feedFile(session: LiveSession, uri: Uri) {
        val name = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { c -> if (c.moveToFirst()) c.getString(0) else null } ?: "file.m4a"
        val pfd = contentResolver.openFileDescriptor(uri, "r") ?: error("Не удалось открыть файл")
        val reader = PcmFileReader.openFd(pfd.detachFd(), name)
        while (!stopRequested.get()) {
            val chunk = reader.nextChunk((CHUNK_BYTES / 2).toUInt())
            if (chunk.isEmpty()) {
                // Файл кончился — ждём «Я вернулся», как при обычной записи.
                SystemClock.sleep(200)
                continue
            }
            session.pushPcm16(chunk)
            SystemClock.sleep(CHUNK_MS / SIMULATION_SPEED)
        }
    }

    private suspend fun publishSnapshots(session: LiveSession) {
        var lastNotification = 0L
        var lastRemote = 0L
        var lastDogNow: Boolean? = null
        while (scope.isActive && !stopRequested.get()) {
            runCatching { session.snapshot() }.getOrNull()?.let { snap ->
                app.live.publish(snap)
                // На другой телефон — раз в 30 с и сразу, когда собака начала или перестала лаять.
                val now = SystemClock.elapsedRealtime()
                if (now - lastRemote >= REMOTE_PERIOD_MS || snap.dogNow != lastDogNow) {
                    lastRemote = now
                    lastDogNow = snap.dogNow
                    startedAtMs = snap.report.startedAt?.unixMs ?: startedAtMs
                    runCatching { session.snapshotJson() }.getOrNull()?.let { json ->
                        app.remote.publish(recording = true, startedAtMs, json, snap)
                    }
                }
                if (now - lastNotification >= NOTIFICATION_PERIOD_MS) {
                    lastNotification = now
                    val text = "Прошло ${Fmt.duration(snap.elapsedMs)} · голос собаки ${Fmt.duration(snap.report.dogMs)}"
                    getSystemService(android.app.NotificationManager::class.java)
                        .notify(NOTIFICATION_ID, notification(text))
                }
            }
            delay(SNAPSHOT_PERIOD_MS)
        }
    }

    private fun notification(text: String?): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, RecordingService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, BarkLogApp.CHANNEL_RECORDING)
            .setSmallIcon(R.drawable.ic_stat_analysis)
            .setContentTitle("Идёт запись")
            .setContentText(text ?: "Слушаю, что происходит дома")
            .setContentIntent(open)
            .addAction(0, "Я вернулся", stop)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    private fun shutdown() {
        wakeLock?.takeIf { it.isHeld }?.release()
        wakeLock = null
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        // Сервис останавливают только после сохранения сессии (см. shutdown).
        stopRequested.set(true)
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "BarkLog"
        private const val ACTION_START = "app.barklog.START"
        private const val ACTION_STOP = "app.barklog.STOP"
        private const val EXTRA_SIMULATE = "simulate"
        private const val NOTIFICATION_ID = 2
        private const val SAMPLE_RATE = 16_000
        private const val CHUNK_MS = 100L
        private const val CHUNK_BYTES = SAMPLE_RATE / 10 * 2
        private const val SIMULATION_SPEED = 20
        private const val SNAPSHOT_PERIOD_MS = 2_000L
        private const val NOTIFICATION_PERIOD_MS = 15_000L
        private const val REMOTE_PERIOD_MS = 30_000L
        /** Wake lock с запасом: даже если что-то пойдёт не так, он отпустится сам. */
        private const val MAX_RECORDING_MS = 24 * 60 * 60 * 1000L

        fun start(context: Context, simulate: Uri? = null) {
            val intent = Intent(context, RecordingService::class.java).setAction(ACTION_START)
            if (simulate != null) intent.putExtra(EXTRA_SIMULATE, simulate.toString())
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            context.startService(Intent(context, RecordingService::class.java).setAction(ACTION_STOP))
        }
    }
}
