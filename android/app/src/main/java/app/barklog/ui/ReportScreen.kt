package app.barklog.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import app.barklog.MainViewModel
import android.net.Uri
import androidx.compose.ui.graphics.Color
import uniffi.barklog_ffi.EpisodeInfo
import uniffi.barklog_ffi.LabelInfo
import uniffi.barklog_ffi.LabelKind
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import uniffi.barklog_ffi.SessionReport
import uniffi.barklog_ffi.StartSource

private const val MAX_EPISODES = 300

@Composable
fun ReportScreen(vm: MainViewModel, id: Long, onBack: () -> Unit, onDeleted: () -> Unit) {
    var report by remember(id) { mutableStateOf<SessionReport?>(null) }
    LaunchedEffect(id) { report = vm.loadReport(id) }
    var confirmDelete by remember { mutableStateOf(false) }
    val c = Bark.colors

    Column(Modifier.fillMaxSize().background(c.bg).safeDrawingPadding()) {
        Row(
            Modifier.fillMaxWidth().height(56.dp).padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier.clip(RoundedCornerShape(8.dp)).clickable(onClick = onBack).padding(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                BackChevron(c.text)
                Spacer(Modifier.width(4.dp))
                Text("Записи", style = Bark.type.body)
            }
            Spacer(Modifier.weight(1f))
            TextButton(onClick = { confirmDelete = true }) {
                Text("Удалить", style = Bark.type.body.copy(color = c.text2))
            }
        }
        report?.let { Report(it, vm, onChanged = { updated -> report = updated }) }
    }

    if (confirmDelete) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text("Удалить отчёт?") },
            text = { Text("Сам аудиофайл останется на телефоне") },
            confirmButton = {
                TextButton(onClick = { confirmDelete = false; onDeleted() }) {
                    Text("Удалить", color = c.danger)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = false }) { Text("Отмена", color = c.text2) }
            },
            containerColor = c.card,
        )
    }
}

@Composable
private fun Report(r: SessionReport, vm: MainViewModel, onChanged: (SessionReport) -> Unit) {
    val c = Bark.colors
    val start = r.startedAt
    var selectedBin by remember(r.id) { mutableStateOf<String?>(null) }

    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Column(Modifier.padding(top = 8.dp, bottom = 12.dp)) {
                Text(if (start != null) Fmt.date(start) else r.sourceName, style = Bark.type.title)
                if (start != null) {
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "${Fmt.hhmm(start, 0)}–${Fmt.hhmm(start, r.durationMs.toLong())}",
                        style = Bark.type.monoLarge,
                    )
                }
                Spacer(Modifier.height(12.dp))
                val summary = if (r.dogMs == 0uL) {
                    "Запись ${Fmt.duration(r.durationMs)}. Собаку не слышно"
                } else {
                    "Запись ${Fmt.duration(r.durationMs)}. Собака подавала голос " +
                        "${Fmt.duration(r.dogMs)} — ${Fmt.percent(r.dogPercent)} времени"
                }
                Text(summary, style = Bark.type.subtitle)
            }
        }

        item {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    StatCard("Голос собаки", Fmt.duration(r.dogMs), "${Fmt.percent(r.dogPercent)} записи", Modifier.weight(1f))
                    StatCard(
                        "Эпизодов", r.episodesCount.toString(),
                        r.longestEpisodeMs?.let { "самый длинный ${Fmt.duration(it)}" } ?: "не было",
                        Modifier.weight(1f),
                    )
                }
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    val first = r.firstDogSoundMs
                    StatCard(
                        "Первый звук",
                        first?.let { Fmt.counter(it) } ?: "—",
                        first?.let { "в ${Fmt.clock(start, it.toLong())}" } ?: "собаку не слышно",
                        Modifier.weight(1f),
                    )
                    StatCard("Самый долгий покой", Fmt.duration(r.longestCalmMs), "без звуков собаки", Modifier.weight(1f))
                }
            }
        }

        item {
            Panel(Modifier.fillMaxWidth()) {
                val bin = r.timelineBinMs.toLong()
                val unit = if (bin >= 60_000) "${bin / 60_000} мин" else "${bin / 1000} с"
                PanelHeader("Сессия", "Столбец — $unit, высота — сколько из них звучала собака")
                Legend(DOG_KINDS)
                Spacer(Modifier.height(16.dp))
                Timeline(r.timeline, start, r.durationMs.toLong(), onSelect = { selectedBin = it })
                Spacer(Modifier.height(8.dp))
                Text(
                    selectedBin ?: "Коснитесь столбца, чтобы увидеть подробности",
                    style = Bark.type.mono.copy(color = if (selectedBin != null) c.text else c.muted),
                )
            }
        }

        item { Shares(r) }
        item { Hours(r) }
        item { EpisodesPanel(r, vm, onChanged) }

        item {
            val note = start?.let {
                when (it.source) {
                    StartSource.MANUAL -> "указано вручную"
                    StartSource.FILE_NAME -> "из имени файла"
                    StartSource.FILE_MODIFIED -> "по времени изменения файла, может быть неточным"
                    StartSource.RECORDED -> "записано в приложении"
                }
            }
            Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(r.sourceName, style = Bark.type.mono.copy(color = c.muted))
                if (note != null) Text("Начало записи: $note", style = Bark.type.mono.copy(color = c.muted))
                Text(
                    "YAMNet, окно 0,96 с · без сети отсеяно ${Fmt.percent(r.gatedPercent)} тихих окон",
                    style = Bark.type.mono.copy(color = c.muted),
                )
            }
        }
    }
}

@Composable
private fun Shares(r: SessionReport) {
    val c = Bark.colors
    Panel(Modifier.fillMaxWidth()) {
        PanelHeader("Из чего состояла запись", "Доля от всего времени и среди звуков собаки")
        StackBar(r.classes.map { c.of(it.kind) to it.percent / 100f }, Modifier.fillMaxWidth())
        Spacer(Modifier.height(12.dp))
        Row(Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
            Text("Звук", style = Bark.type.caption, modifier = Modifier.weight(1f))
            Text("Время", style = Bark.type.caption, modifier = Modifier.widthIn(min = 88.dp), textAlign = TextAlign.End)
            Text("От записи", style = Bark.type.caption, modifier = Modifier.widthIn(min = 76.dp), textAlign = TextAlign.End)
        }
        r.classes.forEach { cls ->
            HorizontalDivider(color = c.line, thickness = 1.dp)
            Row(Modifier.fillMaxWidth().height(40.dp), verticalAlignment = Alignment.CenterVertically) {
                Dot(c.of(cls.kind))
                Spacer(Modifier.width(8.dp))
                Column(Modifier.weight(1f)) {
                    Text(Fmt.label(cls.kind), style = Bark.type.body)
                }
                Text(Fmt.duration(cls.timeMs), style = Bark.type.mono, modifier = Modifier.widthIn(min = 88.dp), textAlign = TextAlign.End)
                Text(Fmt.percent(cls.percent), style = Bark.type.mono, modifier = Modifier.widthIn(min = 76.dp), textAlign = TextAlign.End)
            }
        }
        // Доли среди голоса — отдельной строкой: на узком экране четвёртая колонка не помещается.
        if (r.dogMs > 0uL) {
            Spacer(Modifier.height(12.dp))
            val dogShares = r.classes.filter { (it.dogPercent ?: 0f) > 0f }
                .joinToString(" · ") { "${Fmt.label(it.kind).lowercase()} ${Fmt.percent(it.dogPercent!!)}" }
            Text("Среди голоса: $dogShares", style = Bark.type.caption)
        }
    }
}

@Composable
private fun Hours(r: SessionReport) {
    val c = Bark.colors
    val known = r.startedAt != null
    Panel(Modifier.fillMaxWidth()) {
        PanelHeader("По часам", if (known) "Сколько звучала собака в каждый час" else "Часы от начала записи")
        r.hourly.forEachIndexed { i, b ->
            if (i > 0) HorizontalDivider(color = c.line, thickness = 1.dp)
            val len = (b.endMs - b.startMs).toFloat().coerceAtLeast(1f)
            val dog = DOG_KINDS.sumOf { b.classMs[it.ordinal] }
            Row(Modifier.fillMaxWidth().height(36.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (known) String.format(java.util.Locale.ROOT, "%02d:00", b.hour.toInt()) else "${b.hour.toInt() + 1}-й час",
                    style = Bark.type.mono,
                    modifier = Modifier.widthIn(min = 56.dp),
                )
                ThinBar(
                    DOG_KINDS.map { c.of(it) to b.classMs[it.ordinal].toFloat() / len },
                    Modifier.weight(1f).padding(horizontal = 12.dp),
                )
                Text(
                    if (dog == 0uL) "—" else Fmt.duration(dog),
                    style = Bark.type.mono,
                    modifier = Modifier.widthIn(min = 72.dp),
                    textAlign = TextAlign.End,
                )
            }
        }
    }
}

@Composable
private fun EpisodesPanel(r: SessionReport, vm: MainViewModel, onChanged: (SessionReport) -> Unit) {
    val c = Bark.colors
    val player = rememberEpisodePlayer()
    val scope = rememberCoroutineScope()
    val label: (Long, Long, LabelKind?) -> Unit = { start, end, kind ->
        player.stop()
        scope.launch { vm.labelEpisode(r.id, start, end, kind)?.let(onChanged) }
    }
    // Записи из приложения слушаются из сохранённых фрагментов, файлы — из исходника.
    val source = remember(r.sourceUri, r.id) {
        val u = r.sourceUri
        when {
            u == null -> null
            u.startsWith("clips:") -> ClipSource.Pcm { from, to -> vm.clipPcm(r.id, from, to) }
            else -> ClipSource.File(Uri.parse(u))
        }
    }
    Panel(Modifier.fillMaxWidth()) {
        PanelHeader(
            "Эпизоды",
            "${Fmt.episodes(r.episodes.size)} · паузы до ${r.mergeGapMs.toLong() / 1000} с не разрывают эпизод" +
                if (r.editable) ". Удерживайте эпизод, чтобы поправить, что это было" else "",
        )
        if (r.episodes.isEmpty()) {
            Text("За запись собака не подавала голос", style = Bark.type.caption)
            NotDogList(r, onRestore = { l -> label(l.startMs.toLong(), l.endMs.toLong(), null) })
            return@Panel
        }
        val note = when {
            player.error != null -> player.error
            source == null -> "Для этой записи прослушивание недоступно: у приложения нет постоянного " +
                "доступа к файлу. Чтобы слушать фрагменты, добавьте файл кнопкой «Добавить запись»"
            else -> null
        }
        if (note != null) {
            Text(note, style = Bark.type.caption)
            Spacer(Modifier.height(12.dp))
        }
        r.episodes.take(MAX_EPISODES).forEachIndexed { i, e ->
            if (i > 0) HorizontalDivider(color = c.line, thickness = 1.dp)
            EpisodeRow(
                e = e,
                r = r,
                playing = player.playing == i,
                progress = if (player.playing == i) player.progress else 0f,
                onClick = source?.let { src -> { player.toggle(i, src, e.startMs.toLong(), e.endMs.toLong()) } },
                onLabel = if (r.editable) ({ kind -> label(e.startMs.toLong(), e.endMs.toLong(), kind) }) else null,
            )
        }
        if (r.episodes.size > MAX_EPISODES) {
            Spacer(Modifier.height(8.dp))
            Text("Показаны первые $MAX_EPISODES", style = Bark.type.caption)
        }
        NotDogList(r, onRestore = { l -> label(l.startMs.toLong(), l.endMs.toLong(), null) })
    }
}

/** Отрезки, отмеченные «не собака»: из эпизодов они пропали, но их можно вернуть. */
@Composable
private fun NotDogList(r: SessionReport, onRestore: (LabelInfo) -> Unit) {
    val c = Bark.colors
    val notDog = r.labels.filter { it.label == LabelKind.NOT_DOG }
    if (notDog.isEmpty()) return
    Spacer(Modifier.height(16.dp))
    Text("Отмечено «не собака»", style = Bark.type.caption.copy(color = c.text2))
    Spacer(Modifier.height(4.dp))
    notDog.forEach { l ->
        Row(Modifier.fillMaxWidth().height(40.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                Fmt.clock(r.startedAt, l.startMs.toLong()),
                style = Bark.type.mono.copy(color = c.muted),
                modifier = Modifier.widthIn(min = 72.dp),
            )
            Spacer(Modifier.width(8.dp))
            Text(Fmt.duration((l.endMs - l.startMs).toLong()), style = Bark.type.mono.copy(color = c.muted), modifier = Modifier.weight(1f))
            Text(
                "Вернуть",
                style = Bark.type.body.copy(color = c.text2),
                modifier = Modifier.clip(RoundedCornerShape(6.dp)).clickable { onRestore(l) }.padding(8.dp),
            )
        }
    }
}

private val LABEL_CHOICES = listOf(
    LabelKind.BARK to "Лай",
    LabelKind.WHINE to "Скулёж",
    LabelKind.HOWL to "Вой",
    LabelKind.GROWL to "Рычание",
    LabelKind.NOT_DOG to "Не собака или чужая",
)

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun EpisodeRow(
    e: EpisodeInfo,
    r: SessionReport,
    playing: Boolean,
    progress: Float,
    onClick: (() -> Unit)?,
    onLabel: ((LabelKind?) -> Unit)?,
) {
    val c = Bark.colors
    var menu by remember { mutableStateOf(false) }
    Box(
        Modifier
            .fillMaxWidth()
            .height(44.dp)
            .clip(RoundedCornerShape(8.dp))
            .background(if (playing) c.raised else Color.Transparent)
            .then(
                if (onClick != null || onLabel != null) {
                    Modifier.combinedClickable(
                        onClick = { onClick?.invoke() },
                        onLongClick = onLabel?.let { { menu = true } },
                    )
                } else {
                    Modifier
                },
            ),
    ) {
        Row(
            Modifier.fillMaxSize().padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(Fmt.clock(r.startedAt, e.startMs.toLong()), style = Bark.type.mono, modifier = Modifier.widthIn(min = 72.dp))
            Spacer(Modifier.width(8.dp))
            Dot(c.of(e.kind))
            Spacer(Modifier.width(8.dp))
            Text(Fmt.label(e.kind), style = Bark.type.body)
            if (e.label != null) {
                // Подпись продукта — моноширинная мета: ответ пользователя, а не догадка модели.
                Text("  · отмечено", style = Bark.type.mono.copy(color = c.muted))
            }
            Spacer(Modifier.weight(1f))
            Text(Fmt.duration((e.endMs - e.startMs).toLong()), style = Bark.type.mono)
            if (onClick != null) {
                Spacer(Modifier.width(12.dp))
                PlayGlyph(playing, if (playing) c.text else c.text2)
            }
        }
        if (onLabel != null) {
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }, containerColor = c.card) {
                Text(
                    "Что это было?",
                    style = Bark.type.caption,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
                LABEL_CHOICES.forEach { (kind, text) ->
                    DropdownMenuItem(
                        text = { Text(text, style = Bark.type.body) },
                        onClick = { menu = false; onLabel(kind) },
                    )
                }
                if (e.label != null) {
                    HorizontalDivider(color = c.line)
                    DropdownMenuItem(
                        text = { Text("Снять отметку", style = Bark.type.body.copy(color = c.text2)) },
                        onClick = { menu = false; onLabel(null) },
                    )
                }
            }
        }
        if (playing) {
            // Тонкая полоска прогресса по низу строки.
            Box(
                Modifier
                    .align(Alignment.BottomStart)
                    .fillMaxWidth(progress)
                    .height(2.dp)
                    .background(c.of(e.kind)),
            )
        }
    }
}
