package app.barklog.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.barklog.MainViewModel
import uniffi.barklog_ffi.SessionSummary
import uniffi.barklog_ffi.TrendBucket
import uniffi.barklog_ffi.TrendPeriod

/** Сколько последних периодов показывать на графике. */
private const val MAX_DAYS = 30
private const val MAX_WEEKS = 16

/** Как меняется картина: доля голоса собаки по дням или неделям и сравнение записей. */
@Composable
fun HistoryScreen(vm: MainViewModel, onBack: () -> Unit, onOpen: (Long) -> Unit) {
    val c = Bark.colors
    val sessions by vm.sessions.collectAsStateWithLifecycle()
    var period by rememberSaveable { mutableStateOf(TrendPeriod.WEEK) }
    var buckets by remember { mutableStateOf<List<TrendBucket>?>(null) }
    LaunchedEffect(period, sessions) { buckets = vm.trends(period) }
    var selected by remember(period) { mutableStateOf<Int?>(null) }

    LazyColumn(
        Modifier.fillMaxSize().background(c.bg).safeDrawingPadding(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(
                Modifier.clip(RoundedCornerShape(8.dp)).clickable(onClick = onBack).padding(vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                BackChevron(c.text)
                Spacer(Modifier.width(4.dp))
                Text("Записи", style = Bark.type.body)
            }
            Spacer(Modifier.height(16.dp))
            Text("Динамика", style = Bark.type.title)
            Spacer(Modifier.height(8.dp))
            Text(
                "Какую долю времени записи собака подавала голос. Доля, а не минуты: " +
                    "так записи разной длины можно сравнивать между собой",
                style = Bark.type.subtitle,
            )
            Spacer(Modifier.height(20.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PeriodChip("По неделям", period == TrendPeriod.WEEK) { period = TrendPeriod.WEEK }
                PeriodChip("По дням", period == TrendPeriod.DAY) { period = TrendPeriod.DAY }
            }
        }

        val all = buckets ?: return@LazyColumn
        val shown = all.takeLast(if (period == TrendPeriod.DAY) MAX_DAYS else MAX_WEEKS)
        item {
            Panel(Modifier.fillMaxWidth()) {
                PanelHeader(
                    "Голос собаки, % времени записи",
                    if (period == TrendPeriod.WEEK) "Неделя — с понедельника. Только периоды, когда была запись"
                    else "Только дни, когда была запись",
                )
                TrendChart(shown, selected, onSelect = { selected = it }, height = 180.dp)
                Spacer(Modifier.height(8.dp))
                val sel = selected?.let { shown.getOrNull(it) }
                Text(
                    sel?.let { describe(it, period) } ?: "Коснитесь столбца, чтобы увидеть подробности",
                    style = Bark.type.caption.copy(color = if (sel != null) c.text else c.muted),
                )
            }
        }

        item { PeriodsTable(all.asReversed(), period) }

        val list = sessions.orEmpty()
        if (list.size >= 2) {
            item { Comparison(list, onOpen) }
        }
        if (list.isNotEmpty()) {
            item {
                Spacer(Modifier.height(4.dp))
                TextLink("Выгрузить все записи в CSV", c.text2, vm::shareSessionsCsv)
            }
        }
    }
}

@Composable
private fun PeriodChip(text: String, active: Boolean, onClick: () -> Unit) {
    val c = Bark.colors
    Text(
        text,
        style = Bark.type.body.copy(color = if (active) c.text else c.text2),
        modifier = Modifier
            .clip(RoundedCornerShape(8.dp))
            .background(if (active) c.raised else c.bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 8.dp),
    )
}

/**
 * Столбцы доли голоса по периодам — одна серия акцентным цветом. Шкала округляется
 * вверх до «круглой» доли, чтобы глаз сравнивал высоты, а не гадал о масштабе.
 */
@Composable
fun TrendChart(buckets: List<TrendBucket>, selected: Int?, onSelect: (Int?) -> Unit, height: Dp) {
    val c = Bark.colors
    val max = buckets.maxOfOrNull { it.dogPercent } ?: 0f
    val scale = listOf(5f, 10f, 20f, 30f, 50f, 75f, 100f).firstOrNull { max <= it } ?: 100f
    Column {
        Text("Шкала до ${scale.toInt()} %", style = Bark.type.caption)
        Spacer(Modifier.height(12.dp))
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(height)
                .pointerInput(buckets, selected) {
                    detectTapGestures { pos ->
                        if (buckets.isEmpty()) return@detectTapGestures
                        val i = (pos.x / size.width * buckets.size).toInt().coerceIn(0, buckets.lastIndex)
                        onSelect(if (selected == i) null else i)
                    }
                },
        ) {
            val w = size.width
            val h = size.height
            val dash = PathEffect.dashPathEffect(floatArrayOf(6f, 6f))
            for (g in listOf(0.5f, 1f)) {
                drawLine(c.line, Offset(0f, h - h * g), Offset(w, h - h * g), strokeWidth = 1.dp.toPx(), pathEffect = dash)
            }
            drawLine(c.line, Offset(0f, h), Offset(w, h), strokeWidth = 1.dp.toPx())
            if (buckets.isEmpty()) return@Canvas
            val slot = w / buckets.size
            // Тонкие столбцы: не шире 24 dp, между ними не меньше 2 dp.
            val bw = (slot * 0.6f).coerceAtMost(24.dp.toPx()).coerceAtMost(slot - 2.dp.toPx()).coerceAtLeast(1f)
            val r = 4.dp.toPx().coerceAtMost(bw / 2)
            buckets.forEachIndexed { i, b ->
                if (i == selected) drawRect(c.raised, Offset(i * slot, 0f), Size(slot, h))
                val bh = (b.dogPercent / scale * h).coerceIn(0f, h)
                if (bh <= 0f) return@forEachIndexed
                val x = i * slot + (slot - bw) / 2
                // Скругление только у верхнего края; столбец стоит на базовой линии.
                val path = Path().apply {
                    addRoundRect(
                        RoundRect(
                            left = x, top = h - bh, right = x + bw, bottom = h,
                            topLeftCornerRadius = CornerRadius(r), topRightCornerRadius = CornerRadius(r),
                            bottomLeftCornerRadius = CornerRadius.Zero, bottomRightCornerRadius = CornerRadius.Zero,
                        ),
                    )
                }
                drawPath(path, c.bark)
            }
        }
        if (buckets.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            Row(Modifier.fillMaxWidth()) {
                val first = buckets.first()
                Text(Fmt.dayShort(first.day.toInt(), first.month.toInt()), style = Bark.type.caption, modifier = Modifier.weight(1f))
                if (buckets.size > 1) {
                    val last = buckets.last()
                    Text(Fmt.dayShort(last.day.toInt(), last.month.toInt()), style = Bark.type.caption)
                }
            }
        }
    }
}

private fun title(b: TrendBucket, period: TrendPeriod): String {
    val d = Fmt.day(b.day.toInt(), b.month.toInt())
    return if (period == TrendPeriod.WEEK) "Неделя с $d" else d
}

private fun describe(b: TrendBucket, period: TrendPeriod): String =
    "${title(b, period)} · ${Fmt.records(b.sessions.toInt())} · голос ${Fmt.duration(b.dogMs)} " +
        "из ${Fmt.duration(b.durationMs)} — ${Fmt.percent(b.dogPercent)}"

/** Таблица по периодам — то же, что на графике, цифрами и с разбивкой по звукам. */
@Composable
private fun PeriodsTable(rows: List<TrendBucket>, period: TrendPeriod) {
    val c = Bark.colors
    Panel(Modifier.fillMaxWidth()) {
        PanelHeader(if (period == TrendPeriod.WEEK) "По неделям" else "По дням")
        if (rows.isEmpty()) {
            Text("Записей пока нет", style = Bark.type.caption)
            return@Panel
        }
        rows.forEachIndexed { i, b ->
            if (i > 0) HorizontalDivider(color = c.line, thickness = 1.dp)
            Column(Modifier.fillMaxWidth().padding(vertical = 10.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(title(b, period), style = Bark.type.body, modifier = Modifier.weight(1f))
                    Text(Fmt.percent(b.dogPercent), style = Bark.type.body, textAlign = TextAlign.End)
                }
                Spacer(Modifier.height(2.dp))
                val kinds = DOG_KINDS.mapIndexedNotNull { k, kind ->
                    b.dogClassMs.getOrNull(k)?.takeIf { it > 0u }?.let { "${Fmt.label(kind).lowercase()} ${Fmt.duration(it)}" }
                }
                Text(
                    "${Fmt.records(b.sessions.toInt())} на ${Fmt.duration(b.durationMs)} · " +
                        (kinds.joinToString(", ").ifEmpty { "собаку не слышно" }),
                    style = Bark.type.caption,
                )
            }
        }
    }
}

/** Все записи рядом: доля голоса и заметка — чтобы видеть, что помогает. */
@Composable
private fun Comparison(sessions: List<SessionSummary>, onOpen: (Long) -> Unit) {
    val c = Bark.colors
    val max = sessions.maxOf { it.dogPercent }.coerceAtLeast(1f)
    Panel(Modifier.fillMaxWidth(), padding = 0.dp) {
        Column(Modifier.padding(start = 20.dp, end = 20.dp, top = 20.dp)) {
            PanelHeader("Сравнение записей", "Полоса — доля голоса относительно самой шумной записи. Заметки помогают понять, что сработало")
        }
        sessions.forEachIndexed { i, s ->
            if (i > 0) HorizontalDivider(color = c.line, thickness = 1.dp)
            Column(
                Modifier
                    .fillMaxWidth()
                    .clickable { onOpen(s.id) }
                    .padding(horizontal = 20.dp, vertical = 12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    val start = s.startedAt
                    Text(
                        if (start != null) Fmt.date(start, !Fmt.isCurrentYear(start)) else s.sourceName,
                        style = Bark.type.body,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        // Одинаковая ширина подписи — полосы начинаются с одной линии и сравнимы.
                        modifier = Modifier.width(120.dp),
                    )
                    ThinBar(listOf(c.bark to s.dogPercent / max), Modifier.weight(1f).padding(horizontal = 12.dp))
                    Text(Fmt.percent(s.dogPercent), style = Bark.type.body, textAlign = TextAlign.End, modifier = Modifier.widthIn(min = 56.dp))
                }
                s.note?.let {
                    Spacer(Modifier.height(2.dp))
                    Text(it, style = Bark.type.caption, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}
