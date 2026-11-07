package app.barklog

import android.content.Context
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.SystemClock
import android.provider.OpenableColumns
import android.provider.DocumentsContract
import androidx.core.app.NotificationCompat
import androidx.work.CoroutineWorker
import androidx.work.ForegroundInfo
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.barklog_ffi.BarkLogException
import uniffi.barklog_ffi.ProgressListener
import uniffi.barklog_ffi.SourceFile
import java.util.TimeZone

/**
 * Анализ одной записи в фоне. Работает как foreground-сервис с уведомлением,
 * чтобы система не остановила обработку многочасового файла.
 */
class AnalysisWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {

    override suspend fun getForegroundInfo(): ForegroundInfo = foregroundInfo(null, displayName())

    override suspend fun doWork(): Result {
        val uri = inputData.getString(KEY_URI)?.let(Uri::parse) ?: return Result.failure()
        val meta = queryMeta(uri)
        setProgress(workDataOf(KEY_NAME to meta.name, KEY_PROGRESS to 0f))
        // На Android 12+ запуск foreground-сервиса из фона может быть запрещён —
        // тогда просто работаем дальше без уведомления.
        runCatching { setForeground(foregroundInfo(null, meta.name)) }

        val core = (applicationContext as BarkLogApp).core()
        val pfd = applicationContext.contentResolver.openFileDescriptor(uri, "r")
            ?: return failure("Не удалось открыть файл")

        var lastReport = 0L
        val listener = object : ProgressListener {
            override fun onProgress(processedMs: ULong, totalMs: ULong?): Boolean {
                val now = SystemClock.elapsedRealtime()
                if (now - lastReport >= 500) {
                    lastReport = now
                    val fraction = totalMs?.takeIf { it > 0u }
                        ?.let { (processedMs.toDouble() / it.toDouble()).toFloat().coerceIn(0f, 1f) }
                    setProgressAsync(
                        workDataOf(KEY_NAME to meta.name, KEY_PROGRESS to (fraction ?: -1f)),
                    )
                    if (fraction != null) runCatching { setForegroundAsync(foregroundInfo(fraction, meta.name)) }
                }
                return !isStopped
            }
        }

        val offsetMin = TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60_000
        val result = withContext(Dispatchers.Default) {
            try {
                // Rust забирает дескриптор и закроет его сам.
                val id = core.analyzeFd(
                    pfd.detachFd(),
                    SourceFile(meta.name, meta.modifiedMs, offsetMin, persistentUri(uri)),
                    listener,
                )
                Result.success(workDataOf(KEY_SESSION to id, KEY_NAME to meta.name, KEY_FINISHED_AT to System.currentTimeMillis()))
            } catch (e: BarkLogException.Cancelled) {
                Result.failure()
            } catch (e: BarkLogException) {
                failure(e.cleanMessage(), meta.name)
            }
        }
        deleteSharedCopy(uri)
        return result
    }

    /**
     * Ссылку сохраняем, только если доступ к файлу постоянный (выбран через системный
     * диалог) — по ней потом проигрываются эпизоды.
     */
    private fun persistentUri(uri: Uri): String? {
        val persisted = applicationContext.contentResolver.persistedUriPermissions
            .any { it.uri == uri && it.isReadPermission }
        return uri.toString().takeIf { persisted }
    }

    /** Копию файла, присланного через «Поделиться», после анализа удаляем. */
    private fun deleteSharedCopy(uri: Uri) {
        val path = uri.path ?: return
        if (uri.scheme == "file" && path.startsWith(applicationContext.cacheDir.path)) {
            java.io.File(path).delete()
        }
    }

    private fun failure(message: String, name: String? = null) =
        Result.failure(
            workDataOf(KEY_ERROR to message, KEY_NAME to name, KEY_FINISHED_AT to System.currentTimeMillis()),
        )

    private fun displayName() = inputData.getString(KEY_NAME_HINT) ?: "запись"

    private fun foregroundInfo(fraction: Float?, name: String): ForegroundInfo {
        val notification = NotificationCompat.Builder(applicationContext, BarkLogApp.CHANNEL_ANALYSIS)
            .setSmallIcon(R.drawable.ic_stat_analysis)
            .setContentTitle("Анализ записи")
            .setContentText(name)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setProgress(100, ((fraction ?: 0f) * 100).toInt(), fraction == null)
            .build()
        val type = if (Build.VERSION.SDK_INT >= 35) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROCESSING
        } else {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
        }
        return ForegroundInfo(NOTIFICATION_ID, notification, type)
    }

    private data class Meta(val name: String, val modifiedMs: Long?)

    private fun queryMeta(uri: Uri): Meta {
        var name = uri.lastPathSegment ?: "запись"
        var modified: Long? = null
        val cols = arrayOf(OpenableColumns.DISPLAY_NAME, DocumentsContract.Document.COLUMN_LAST_MODIFIED)
        runCatching {
            applicationContext.contentResolver.query(uri, cols, null, null, null)?.use { c ->
                if (c.moveToFirst()) {
                    c.getColumnIndex(OpenableColumns.DISPLAY_NAME).takeIf { it >= 0 && !c.isNull(it) }
                        ?.let { name = c.getString(it) }
                    c.getColumnIndex(DocumentsContract.Document.COLUMN_LAST_MODIFIED)
                        .takeIf { it >= 0 && !c.isNull(it) }
                        ?.let { modified = c.getLong(it).takeIf { ms -> ms > 0 } }
                }
            }
        }
        return Meta(name, modified)
    }

    companion object {
        const val UNIQUE_NAME = "analysis"
        const val KEY_URI = "uri"
        const val KEY_NAME_HINT = "name_hint"
        const val KEY_NAME = "name"
        const val KEY_PROGRESS = "progress"
        const val KEY_SESSION = "session"
        const val KEY_ERROR = "error"
        /** Порядок завершённых задач WorkManager не гарантирует — сортируем по этому полю. */
        const val KEY_FINISHED_AT = "finished_at"
        private const val NOTIFICATION_ID = 1
    }
}

/** Текст ошибки для пользователя. */
fun BarkLogException.cleanMessage(): String = when (this) {
    is BarkLogException.Audio -> reason
    is BarkLogException.Model -> reason
    is BarkLogException.Storage -> reason
    is BarkLogException.Cancelled -> "Анализ отменён"
    is BarkLogException.NotFound -> "Отчёт не найден"
    is BarkLogException.Training -> reason
}
