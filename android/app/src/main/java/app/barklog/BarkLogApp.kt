package app.barklog

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import app.barklog.remote.RemoteSync
import uniffi.barklog_ffi.BarkLog
import java.io.File

class BarkLogApp : Application() {

    /** Состояние живой записи (сервис пишет, экраны читают). */
    val live = LiveController()

    /** Удалённый просмотр с другого телефона (Firebase). */
    val remote: RemoteSync by lazy { RemoteSync.create(this) }

    private val coreLock = Mutex()
    @Volatile private var core: BarkLog? = null

    override fun onCreate() {
        super.onCreate()
        val channel = NotificationChannel(
            CHANNEL_ANALYSIS,
            getString(R.string.channel_analysis),
            NotificationManager.IMPORTANCE_LOW,
        )
        val recording = NotificationChannel(
            CHANNEL_RECORDING,
            getString(R.string.channel_recording),
            NotificationManager.IMPORTANCE_LOW,
        )
        val alerts = NotificationChannel(
            CHANNEL_ALERTS,
            getString(R.string.channel_alerts),
            NotificationManager.IMPORTANCE_HIGH,
        )
        getSystemService(NotificationManager::class.java)
            .createNotificationChannels(listOf(channel, recording, alerts))
        remote // инициализируем Firebase заранее, чтобы вход восстановился до первого экрана
    }

    /**
     * Ядро на Rust: модель YAMNet + база сессий. Создаётся один раз, при первом
     * обращении — загрузка модели занимает доли секунды, но не на главном потоке.
     */
    suspend fun core(): BarkLog {
        core?.let { return it }
        return coreLock.withLock {
            core ?: withContext(Dispatchers.IO) {
                val model = ensureModel()
                val db = File(filesDir, "barklog.db")
                BarkLog(db.absolutePath, model.absolutePath)
            }.also { core = it }
        }
    }

    /** Rust читает модель по пути, поэтому копируем её из APK в файлы приложения. */
    private fun ensureModel(): File {
        val target = File(filesDir, "models/$MODEL_ASSET")
        val assetSize = assets.openFd(MODEL_ASSET).use { it.length }
        if (target.length() != assetSize) {
            target.parentFile?.mkdirs()
            val tmp = File(target.path + ".tmp")
            assets.open(MODEL_ASSET).use { input -> tmp.outputStream().use { input.copyTo(it) } }
            tmp.renameTo(target)
        }
        return target
    }

    companion object {
        const val CHANNEL_ANALYSIS = "analysis"
        const val CHANNEL_RECORDING = "recording"
        const val CHANNEL_ALERTS = "alerts"
        private const val MODEL_ASSET = "yamnet.safetensors"
    }
}
