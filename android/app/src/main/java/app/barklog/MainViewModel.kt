package app.barklog

import android.app.Activity
import android.app.Application
import androidx.credentials.exceptions.GetCredentialCancellationException
import app.barklog.remote.RemoteLive
import app.barklog.remote.RemoteSync
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.flatMapLatest
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
import uniffi.barklog_ffi.LabelKind
import uniffi.barklog_ffi.PersonalStatus
import uniffi.barklog_ffi.SensitivityLimits
import uniffi.barklog_ffi.SensitivitySettings
import uniffi.barklog_ffi.sensitivityLimits as coreSensitivityLimits
import uniffi.barklog_ffi.SessionReport
import uniffi.barklog_ffi.SessionSummary
import uniffi.barklog_ffi.TrendBucket
import uniffi.barklog_ffi.TrendPeriod
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

    /** Удалённый просмотр: аккаунт и запись на телефонах аккаунта. */
    val remoteAvailable: Boolean get() = barkApp.remote.available
    val remoteEmulator: Boolean get() = barkApp.remote.emulator
    val account: StateFlow<RemoteSync.Account?> = barkApp.remote.user

    @OptIn(ExperimentalCoroutinesApi::class)
    val remoteLive: StateFlow<RemoteLive?> = barkApp.remote.user
        .flatMapLatest { barkApp.remote.observe() }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)

    fun signIn(activity: Activity) {
        viewModelScope.launch {
            try {
                barkApp.remote.signIn(activity)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // Отмена входа пользователем — не ошибка.
                if (e !is GetCredentialCancellationException) {
                    _error.value = "Не удалось войти: ${e.message ?: e.javaClass.simpleName}"
                }
            }
        }
    }

    fun signOut() = barkApp.remote.signOut()

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

    /** Личная модель: сколько отмечено, обучена ли, точность. */
    private val _personal = MutableStateFlow<PersonalStatus?>(null)
    val personal: StateFlow<PersonalStatus?> = _personal.asStateFlow()

    private val _training = MutableStateFlow(false)
    val training: StateFlow<Boolean> = _training.asStateFlow()

    /** Чувствительность: порог «это собака» и правила эпизодов. null — ещё загружается. */
    private val _sensitivity = MutableStateFlow<SensitivitySettings?>(null)
    val sensitivity: StateFlow<SensitivitySettings?> = _sensitivity.asStateFlow()

    val sensitivityLimits: SensitivityLimits by lazy { coreSensitivityLimits() }

    /** Идёт пересчёт записей после смены чувствительности. */
    private val _applying = MutableStateFlow(false)
    val applying: StateFlow<Boolean> = _applying.asStateFlow()

    init {
        refresh()
        viewModelScope.launch {
            runCatching {
                val core = barkApp.core()
                _sensitivity.value = withContext(Dispatchers.IO) { core.sensitivity() }
            }
        }
    }

    /** Сохраняет чувствительность и пересчитывает все записи. */
    fun setSensitivity(settings: SensitivitySettings) {
        viewModelScope.launch {
            _applying.value = true
            try {
                val core = barkApp.core()
                _sensitivity.value = withContext(Dispatchers.Default) { core.setSensitivity(settings) }
                refresh()
            } catch (e: BarkLogException) {
                _error.value = e.cleanMessage()
            } finally {
                _applying.value = false
            }
        }
    }

    private fun refreshPersonal() {
        viewModelScope.launch {
            runCatching {
                val core = barkApp.core()
                _personal.value = withContext(Dispatchers.IO) { core.personalStatus() }
            }
        }
    }

    /** Ответ на эпизод; возвращает пересчитанный отчёт. */
    suspend fun labelEpisode(sessionId: Long, startMs: Long, endMs: Long, label: LabelKind?): SessionReport? =
        try {
            val core = barkApp.core()
            withContext(Dispatchers.IO) {
                core.labelEpisode(sessionId, startMs.toULong(), endMs.toULong(), label)
            }.also {
                refresh()
            }
        } catch (e: BarkLogException) {
            _error.value = e.cleanMessage()
            null
        }

    fun trainPersonal() {
        viewModelScope.launch {
            _training.value = true
            try {
                val core = barkApp.core()
                _personal.value = withContext(Dispatchers.Default) { core.trainPersonal() }
                refresh()
            } catch (e: BarkLogException) {
                _error.value = e.cleanMessage()
            } finally {
                _training.value = false
            }
        }
    }

    fun resetPersonal() {
        viewModelScope.launch {
            _training.value = true
            try {
                val core = barkApp.core()
                withContext(Dispatchers.Default) { core.resetPersonal() }
                refresh()
            } catch (e: BarkLogException) {
                _error.value = e.cleanMessage()
            } finally {
                _training.value = false
            }
        }
    }

    fun refresh() {
        viewModelScope.launch {
            try {
                val core = barkApp.core()
                _sessions.value = withContext(Dispatchers.IO) { core.listSessions() }
                refreshPersonal()
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

    /** Сохраняет заметку к сессии; пустая убирает её. Возвращает сохранённый текст или null при ошибке. */
    suspend fun setNote(id: Long, note: String): Result<String?> = try {
        val core = barkApp.core()
        Result.success(withContext(Dispatchers.IO) { core.setNote(id, note) }).also { refresh() }
    } catch (e: BarkLogException) {
        _error.value = e.cleanMessage()
        Result.failure(e)
    }

    /** Записи по дням или неделям, от старых к новым; пустой список при ошибке. */
    suspend fun trends(period: TrendPeriod): List<TrendBucket> = try {
        val core = barkApp.core()
        val offsetMin = java.util.TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60_000
        withContext(Dispatchers.IO) { core.trends(period, offsetMin) }
    } catch (e: BarkLogException) {
        _error.value = e.cleanMessage()
        emptyList()
    }

    /** Готовит и отправляет PDF-отчёт о записи. */
    fun sharePdf(r: SessionReport) = export("Не удалось подготовить PDF") {
        Export.writePdf(barkApp, r) to "application/pdf"
    }

    /** Таблица эпизодов записи в CSV. */
    fun shareEpisodesCsv(r: SessionReport) = export("Не удалось подготовить таблицу") {
        val csv = barkApp.core().exportEpisodesCsv(r.id)
        Export.writeCsv(barkApp, "${Export.baseName(r)}-episodes", csv) to "text/csv"
    }

    /** Все записи в CSV. */
    fun shareSessionsCsv() = export("Не удалось подготовить таблицу") {
        Export.writeCsv(barkApp, "barklog-sessions", barkApp.core().exportSessionsCsv()) to "text/csv"
    }

    private val _exporting = MutableStateFlow(false)
    val exporting: StateFlow<Boolean> = _exporting.asStateFlow()

    private fun export(failure: String, make: suspend () -> Pair<java.io.File, String>) {
        viewModelScope.launch {
            _exporting.value = true
            try {
                val (file, mime) = withContext(Dispatchers.IO) { make() }
                Export.share(barkApp, file, mime, file.name)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                _error.value = "$failure: ${(e as? BarkLogException)?.cleanMessage() ?: e.message ?: e.javaClass.simpleName}"
            } finally {
                _exporting.value = false
            }
        }
    }

    fun delete(id: Long) {
        viewModelScope.launch {
            val core = barkApp.core()
            withContext(Dispatchers.IO) { core.deleteSession(id) }
            refresh()
        }
    }
}
