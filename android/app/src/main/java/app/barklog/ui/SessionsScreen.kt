package app.barklog.ui

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.work.WorkInfo
import app.barklog.AnalysisState
import app.barklog.MainActivity
import app.barklog.MainViewModel
import app.barklog.LiveState
import app.barklog.RecordingService
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import uniffi.barklog_ffi.SessionSummary
import kotlin.math.roundToInt

@Composable
fun SessionsScreen(vm: MainViewModel, onOpen: (Long) -> Unit, onOpenLive: () -> Unit) {
    val sessions by vm.sessions.collectAsStateWithLifecycle()
    val analysis by vm.analysis.collectAsStateWithLifecycle()
    val error by vm.error.collectAsStateWithLifecycle()
    val dismissed by vm.dismissedAnalysis.collectAsStateWithLifecycle()
    val live by vm.live.collectAsStateWithLifecycle()
    val c = Bark.colors
    val context = LocalContext.current

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) vm.analyzePicked(uri)
    }
    var askedNotifications by rememberSaveable { mutableStateOf(false) }
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        // Без уведомлений анализ всё равно идёт, просто не видно прогресса в шторке.
        picker.launch(AUDIO_TYPES)
    }
    val add = {
        if (MainActivity.NEEDS_NOTIFICATION_PERMISSION && !askedNotifications) {
            askedNotifications = true
            notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            picker.launch(AUDIO_TYPES)
        }
    }

    // Запись: сначала микрофон (и уведомления), потом сервис и экран записи.
    val startRecording = {
        RecordingService.start(context)
        onOpenLive()
    }
    val micPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { granted ->
        if (granted[Manifest.permission.RECORD_AUDIO] == true) startRecording()
        else vm.showError("Без доступа к микрофону запись невозможна — его можно разрешить в настройках приложения")
    }
    val record = {
        val perms = buildList {
            add(Manifest.permission.RECORD_AUDIO)
            if (MainActivity.NEEDS_NOTIFICATION_PERMISSION) add(Manifest.permission.POST_NOTIFICATIONS)
        }
        micPermission.launch(perms.toTypedArray())
    }
    // Отладка: «запись» из файла вместо микрофона (долгое нажатие на кнопку в debug-сборке).
    val simulatePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            RecordingService.start(context, simulate = uri)
            onOpenLive()
        }
    }
    val debuggable = remember {
        context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0
    }

    // Когда анализ закончился — обновляем список.
    LaunchedEffect(analysis?.id, analysis?.state) {
        if (analysis?.state == WorkInfo.State.SUCCEEDED) vm.refresh()
    }

    LazyColumn(
        Modifier.fillMaxSize().background(c.bg).safeDrawingPadding(),
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 16.dp),
    ) {
        item {
            Row(Modifier.fillMaxWidth().height(40.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("BarkLog", style = Bark.type.heading)
            }
            Spacer(Modifier.height(24.dp))
            Text("Записи", style = Bark.type.title)
            Spacer(Modifier.height(8.dp))
            Text(
                "Уходя, включите запись и оставьте телефон рядом с собакой — " +
                    "приложение покажет, когда и сколько она лаяла и скулила",
                style = Bark.type.subtitle,
            )
            Spacer(Modifier.height(20.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                val recording = live.isBusy()
                PrimaryAction(
                    text = if (recording) "Идёт запись" else "Начать запись",
                    onClick = if (recording) onOpenLive else record,
                    onLongClick = if (debuggable && !recording) ({ simulatePicker.launch(AUDIO_TYPES) }) else null,
                )
                SecondaryAction("Добавить файл", onClick = add)
            }
            Spacer(Modifier.height(24.dp))
        }

        error?.let { message ->
            item {
                Panel(Modifier.fillMaxWidth()) {
                    Text(message, style = Bark.type.body.copy(color = c.danger))
                    TextButton(onClick = vm::dismissError) { Text("Скрыть", color = c.text2) }
                }
                Spacer(Modifier.height(12.dp))
            }
        }

        analysis?.takeIf { it.visible() && !(it.state.isFinished && it.id == dismissed) }?.let { a ->
            item {
                AnalysisCard(
                    a,
                    onCancel = vm::cancelAnalysis,
                    onOpen = {
                        vm.dismissAnalysis(a.id)
                        a.sessionId?.let(onOpen)
                    },
                )
                Spacer(Modifier.height(12.dp))
            }
        }

        val list = sessions
        when {
            list == null -> Unit
            list.isEmpty() -> item {
                Panel(Modifier.fillMaxWidth()) {
                    Text("Записей пока нет", style = Bark.type.heading)
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "Подойдёт файл из стандартного диктофона: m4a, mp3, wav, flac или ogg. " +
                            "Анализ идёт на телефоне, запись никуда не отправляется",
                        style = Bark.type.caption,
                    )
                }
            }
            else -> {
                item {
                    Panel(Modifier.fillMaxWidth(), padding = 0.dp) {
                        list.forEachIndexed { i, s ->
                            if (i > 0) HorizontalDivider(color = c.line, thickness = 1.dp)
                            SessionRow(s, onClick = { onOpen(s.id) })
                        }
                    }
                }
            }
        }
    }
}

private val AUDIO_TYPES = arrayOf("audio/*", "application/ogg", "video/mp4")

private fun LiveState.isBusy() = this is LiveState.Starting || this is LiveState.Recording || this is LiveState.Saving

/** Главная кнопка: заливка цветом текста, 44 dp. Долгое нажатие — только для отладки. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PrimaryAction(text: String, onClick: () -> Unit, onLongClick: (() -> Unit)? = null) {
    val c = Bark.colors
    Box(
        Modifier
            .height(44.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(c.primary)
            .combinedClickable(onClick = onClick, onLongClick = onLongClick)
            .padding(horizontal = 20.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = Bark.type.body.copy(color = c.onPrimary))
    }
}

/** Второстепенная кнопка: приподнятая поверхность с волосяной рамкой. */
@Composable
private fun SecondaryAction(text: String, onClick: () -> Unit) {
    val c = Bark.colors
    Box(
        Modifier
            .height(44.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(c.card)
            .border(1.dp, c.line, RoundedCornerShape(10.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 20.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = Bark.type.body)
    }
}

/** Карточку завершённого анализа показываем, только если он закончился ошибкой или сессией. */
private fun AnalysisState.visible() = !state.isFinished ||
    (state == WorkInfo.State.FAILED && error != null) ||
    (state == WorkInfo.State.SUCCEEDED && sessionId != null)

@Composable
private fun AnalysisCard(a: AnalysisState, onCancel: () -> Unit, onOpen: () -> Unit) {
    val c = Bark.colors
    Panel(Modifier.fillMaxWidth()) {
        val title = when (a.state) {
            WorkInfo.State.SUCCEEDED -> "Готово"
            WorkInfo.State.FAILED -> "Не удалось проанализировать"
            WorkInfo.State.ENQUEUED, WorkInfo.State.BLOCKED -> "В очереди"
            else -> "Анализ"
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, style = Bark.type.heading, modifier = Modifier.weight(1f))
            a.progress?.takeIf { !a.state.isFinished }?.let {
                Text("${(it * 100).roundToInt()} %", style = Bark.type.mono.copy(color = c.text2))
            }
        }
        a.name?.let {
            Spacer(Modifier.height(4.dp))
            Text(it, style = Bark.type.caption, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (!a.state.isFinished) {
            Spacer(Modifier.height(16.dp))
            val p = a.progress
            if (p == null) {
                LinearProgressIndicator(Modifier.fillMaxWidth().clip(RoundedCornerShape(2.dp)), color = c.bark, trackColor = c.raised)
            } else {
                LinearProgressIndicator(
                    progress = { p },
                    modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(2.dp)),
                    color = c.bark,
                    trackColor = c.raised,
                    gapSize = 0.dp,
                    drawStopIndicator = {},
                )
            }
            Spacer(Modifier.height(12.dp))
            TextLink("Отменить", c.text2, onCancel)
        } else if (a.state == WorkInfo.State.FAILED) {
            Spacer(Modifier.height(8.dp))
            Text(a.error.orEmpty(), style = Bark.type.body.copy(color = c.danger))
        } else {
            Spacer(Modifier.height(12.dp))
            TextLink("Открыть отчёт", c.text, onOpen)
        }
    }
}

@Composable
private fun SessionRow(s: SessionSummary, onClick: () -> Unit) {
    val c = Bark.colors
    val start = s.startedAt
    val title = if (start != null) {
        val withYear = !Fmt.isCurrentYear(start)
        "${Fmt.date(start, withYear)}, ${Fmt.hhmm(start, 0)}–${Fmt.hhmm(start, s.durationMs.toLong())}"
    } else {
        s.sourceName
    }
    val subtitle = if (s.dogMs == 0uL) {
        "Собаку не слышно"
    } else {
        "Голос ${Fmt.duration(s.dogMs)} · ${Fmt.percent(s.dogPercent)} · ${Fmt.episodes(s.episodes.toInt())}"
    }
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .heightIn(min = 64.dp)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = Bark.type.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Spacer(Modifier.height(2.dp))
            Text(subtitle, style = Bark.type.caption, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.width(12.dp))
        Box(contentAlignment = Alignment.CenterEnd) {
            Text(Fmt.duration(s.durationMs), style = Bark.type.mono.copy(color = c.text2))
        }
    }
}

/** Текстовая кнопка без минимальной высоты 48 dp у TextButton — она раздувала карточку снизу. */
@Composable
private fun TextLink(text: String, color: androidx.compose.ui.graphics.Color, onClick: () -> Unit) {
    Text(
        text,
        style = Bark.type.body.copy(color = color),
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .clickable(onClick = onClick)
            .padding(vertical = 4.dp),
    )
}
