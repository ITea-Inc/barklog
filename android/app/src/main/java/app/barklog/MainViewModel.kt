package app.barklog

import android.app.Application
import android.content.Intent
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.workDataOf
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.barklog_ffi.BarkLogException
import uniffi.barklog_ffi.SessionReport
import uniffi.barklog_ffi.SessionSummary
import java.util.UUID

/** Текущий анализ для карточки на главном экране. */
data class AnalysisState(
    val id: UUID,
    val name: String?,
    /** 0..1, или null, если длительность файла неизвестна. */
    val progress: Float?,
    val state: WorkInfo.State,
    val error: String?,
    val sessionId: Long?,
)

class MainViewModel(app: Application) : AndroidViewModel(app) {

    private val barkApp = app as BarkLogApp
    private val work = WorkManager.getInstance(app)

    private val _sessions = MutableStateFlow<List<SessionSummary>?>(null)
    /** null — ещё загружается. */
    val sessions: StateFlow<List<SessionSummary>?> = _sessions.asStateFlow()

    private val _dismissed = MutableStateFlow<UUID?>(null)
    /** Карточка завершённого анализа, которую пользователь уже видел (открыл отчёт). */
    val dismissedAnalysis: StateFlow<UUID?> = _dismissed.asStateFlow()

    fun dismissAnalysis(id: UUID) {
        _dismissed.value = id
    }

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    /** Живая запись «Я ушёл». */
    val live: StateFlow<LiveState> = barkApp.live.state

    fun stopRecording() = RecordingService.stop(barkApp)

    fun acknowledgeLive() = barkApp.live.acknowledge()

    fun showError(message: String) {
        _error.value = message
    }

    /** Звук фрагмента сессии, записанной в приложении: PCM 16 бит, 16 кГц. */
    suspend fun clipPcm(sessionId: Long, startMs: Long, endMs: Long): ByteArray {
        val core = barkApp.core()
        return withContext(Dispatchers.IO) {
            core.clipPcm16(sessionId, startMs.toULong(), endMs.toULong())
        }
    }

    val analysis: StateFlow<AnalysisState?> = work.getWorkInfosForUniqueWorkFlow(AnalysisWorker.UNIQUE_NAME)
        .map { infos ->
            // Сначала — то, что идёт сейчас, иначе самый свежий из завершённых.
            val info = infos.firstOrNull { !it.state.isFinished }
                ?: infos.maxByOrNull { it.outputData.getLong(AnalysisWorker.KEY_FINISHED_AT, 0) }
            info?.let {
                val data = if (it.state.isFinished) it.outputData else it.progress
                val p = data.getFloat(AnalysisWorker.KEY_PROGRESS, -1f)
                AnalysisState(
                    id = it.id,
                    name = it.progress.getString(AnalysisWorker.KEY_NAME)
                        ?: it.outputData.getString(AnalysisWorker.KEY_NAME),
                    progress = p.takeIf { v -> v >= 0f },
                    state = it.state,
                    error = it.outputData.getString(AnalysisWorker.KEY_ERROR),
                    sessionId = it.outputData.getLong(AnalysisWorker.KEY_SESSION, -1).takeIf { id -> id >= 0 },
                )
            }
        }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            try {
                val core = barkApp.core()
                _sessions.value = withContext(Dispatchers.IO) { core.listSessions() }
            } catch (e: BarkLogException) {
                _error.value = e.cleanMessage()
                _sessions.value = emptyList()
            }
        }
    }

    /** Файл, выбранный через системный диалог (ACTION_OPEN_DOCUMENT). */
    fun analyzePicked(uri: Uri) {
        runCatching {
            barkApp.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        enqueue(uri)
    }

    /**
     * Файл, присланный через «Поделиться». Разрешение на чтение живёт только пока жива
     * Activity, поэтому сначала копируем запись к себе.
     */
    fun analyzeShared(uri: Uri) {
        viewModelScope.launch {
            val copy = withContext(Dispatchers.IO) {
                runCatching {
                    val name = uri.lastPathSegment?.substringAfterLast('/') ?: "запись"
                    val dir = barkApp.cacheDir.resolve("shared").apply { mkdirs() }
                    val target = dir.resolve(name)
                    barkApp.contentResolver.openInputStream(uri)!!.use { input ->
                        target.outputStream().use { input.copyTo(it) }
                    }
                    Uri.fromFile(target)
                }.getOrNull()
            }
            if (copy == null) _error.value = "Не удалось прочитать присланный файл" else enqueue(copy)
        }
    }

    private fun enqueue(uri: Uri) {
        val request = OneTimeWorkRequestBuilder<AnalysisWorker>()
            .setInputData(workDataOf(AnalysisWorker.KEY_URI to uri.toString()))
            .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
            .build()
        // Несколько файлов подряд анализируются по очереди.
        work.enqueueUniqueWork(AnalysisWorker.UNIQUE_NAME, ExistingWorkPolicy.APPEND_OR_REPLACE, request)
    }

    fun cancelAnalysis() {
        work.cancelUniqueWork(AnalysisWorker.UNIQUE_NAME)
    }

    fun dismissError() {
        _error.value = null
    }

    suspend fun loadReport(id: Long): SessionReport? = try {
        val core = barkApp.core()
        withContext(Dispatchers.IO) { core.getReport(id) }
    } catch (e: BarkLogException) {
        _error.value = e.cleanMessage()
        null
    }

    fun delete(id: Long) {
        viewModelScope.launch {
            val core = barkApp.core()
            withContext(Dispatchers.IO) { core.deleteSession(id) }
            refresh()
        }
    }
}
