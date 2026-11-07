package app.barklog.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import uniffi.barklog_ffi.SoundKind
import uniffi.barklog_ffi.StartInfo
import uniffi.barklog_ffi.TimeBucket
import kotlin.math.roundToInt

val DOG_KINDS = listOf(SoundKind.BARK, SoundKind.WHINE, SoundKind.HOWL, SoundKind.GROWL)

/** Карточка: поверхность на шаг светлее фона, волосяная рамка, радиус 12. */
@Composable
fun Panel(modifier: Modifier = Modifier, padding: Dp = 20.dp, content: @Composable ColumnScope.() -> Unit) {
    val c = Bark.colors
    Column(
        modifier
            .clip(RoundedCornerShape(12.dp))
            .background(c.card)
            .border(1.dp, c.line, RoundedCornerShape(12.dp))
            .padding(padding),
        content = content,
    )
}

@Composable
fun PanelHeader(title: String, note: String? = null) {
    Text(title, style = Bark.type.heading)
    if (note != null) {
        Spacer(Modifier.height(4.dp))
        Text(note, style = Bark.type.caption)
    }
    Spacer(Modifier.height(16.dp))
}

@Composable
fun Dot(color: Color, modifier: Modifier = Modifier) {
    Box(modifier.size(8.dp).clip(CircleShape).background(color))
}

@Composable
fun StatCard(label: String, value: String, note: String, modifier: Modifier = Modifier) {
    Panel(modifier, padding = 16.dp) {
        Text(label, style = Bark.type.caption.copy(color = Bark.colors.text2))
        Spacer(Modifier.height(12.dp))
        Text(value, style = Bark.type.figure, maxLines = 1)
        Spacer(Modifier.height(8.dp))
        Text(note, style = Bark.type.caption, maxLines = 2)
    }
}

@Composable
fun Legend(kinds: List<SoundKind>) {
    Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        kinds.forEach {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Dot(Bark.colors.of(it))
                Spacer(Modifier.width(8.dp))
                Text(Fmt.label(it), style = Bark.type.caption.copy(color = Bark.colors.text2))
            }
        }
    }
}

/** Тонкая полоса: доли по классам относительно `total`. */
@Composable
fun ThinBar(parts: List<Pair<Color, Float>>, modifier: Modifier = Modifier, height: Dp = 6.dp) {
    val c = Bark.colors
    Row(
        modifier
            .height(height)
            .clip(RoundedCornerShape(height / 2))
            .background(c.raised),
    ) {
        val shown = parts.filter { it.second > 0f }
        shown.forEach { (color, frac) ->
            Box(Modifier.fillMaxHeight().weight(frac.coerceIn(0.001f, 1f)).background(color))
        }
        val rest = 1f - shown.sumOf { it.second.toDouble() }.toFloat()
        if (rest > 0.001f) Spacer(Modifier.weight(rest))
    }
}

/** Одна полоса на 100 %: из чего состояла запись. */
@Composable
fun StackBar(parts: List<Pair<Color, Float>>, modifier: Modifier = Modifier) {
    Row(
        modifier.height(12.dp).clip(RoundedCornerShape(4.dp)),
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        parts.filter { it.second > 0f }.forEach { (color, weight) ->
            Box(Modifier.fillMaxHeight().weight(weight.coerceAtLeast(0.002f)).background(color))
        }
    }
}

/**
 * Главный график: столбец на интервал, высота — какую долю интервала звучала собака.
 * Касание выбирает столбец; его описание показывается в [onSelect].
 */
@Composable
fun Timeline(
    bins: List<TimeBucket>,
    start: StartInfo?,
    totalMs: Long,
    modifier: Modifier = Modifier,
    onSelect: (String?) -> Unit,
) {
    val c = Bark.colors
    val fractions = remember(bins) {
        bins.map { b ->
            val len = (b.endMs - b.startMs).toFloat().coerceAtLeast(1f)
            DOG_KINDS.map { k -> b.classMs[k.ordinal].toFloat() / len }
        }
    }
    val max = fractions.maxOfOrNull { it.sum() } ?: 0f
    val scale = listOf(0.1f, 0.25f, 0.5f, 1f).firstOrNull { max <= it } ?: 1f
    var selected by remember(bins) { mutableStateOf<Int?>(null) }

    Column(modifier) {
        Text(
            "Шкала до ${Fmt.percent(scale * 100).replace(",0", "")} интервала",
            style = Bark.type.caption,
        )
        Spacer(Modifier.height(12.dp))
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(200.dp)
                .pointerInput(bins) {
                    detectTapGestures { pos ->
                        if (bins.isEmpty()) return@detectTapGestures
                        val i = (pos.x / size.width * bins.size).toInt().coerceIn(0, bins.lastIndex)
                        selected = if (selected == i) null else i
                        onSelect(selected?.let { describe(bins[it], start) })
                    }
                },
        ) {
            val w = size.width
            val h = size.height
            val dash = PathEffect.dashPathEffect(floatArrayOf(6f, 6f))
            for (g in listOf(0.5f, 1f)) {
                val y = h - h * g
                drawLine(c.line, Offset(0f, y), Offset(w, y), strokeWidth = 1.dp.toPx(), pathEffect = dash)
            }
            drawLine(c.line, Offset(0f, h), Offset(w, h), strokeWidth = 1.dp.toPx())
            if (bins.isEmpty()) return@Canvas
            val bw = w / bins.size
            val gap = if (bins.size <= 120) bw * 0.18f else 0f
            fractions.forEachIndexed { i, parts ->
                var top = h
                if (i == selected) {
                    drawRect(c.raised, Offset(i * bw, 0f), Size(bw, h))
                }
                parts.forEachIndexed { k, f ->
                    if (f <= 0f) return@forEachIndexed
                    val bh = (f / scale * h).coerceAtMost(top)
                    top -= bh
                    drawRect(
                        c.of(DOG_KINDS[k]),
                        Offset(i * bw + gap / 2, top),
                        Size((bw - gap).coerceAtLeast(1f), bh),
                    )
                }
            }
        }
        Spacer(Modifier.height(8.dp))
        AxisLabels(Fmt.axisTicks(start, totalMs), totalMs)
    }
}

private fun describe(b: TimeBucket, start: StartInfo?): String {
    val parts = DOG_KINDS.mapNotNull { k ->
        b.classMs[k.ordinal].takeIf { it > 0u }?.let { "${Fmt.label(k).lowercase()} ${Fmt.duration(it)}" }
    }
    val time = Fmt.clock(start, b.startMs.toLong()).take(5)
    return "$time · " + (parts.joinToString(", ").ifEmpty { "тихо" })
}

@Composable
private fun AxisLabels(ticks: List<Pair<Long, String>>, totalMs: Long) {
    BoxWithConstraints(Modifier.fillMaxWidth().height(16.dp)) {
        val width = constraints.maxWidth
        ticks.forEach { (t, label) ->
            val x = (t.toFloat() / totalMs.coerceAtLeast(1) * width).roundToInt()
            Text(
                label,
                style = Bark.type.mono.copy(color = Bark.colors.muted),
                maxLines = 1,
                modifier = Modifier.layout { m, c ->
                    val p = m.measure(c.copy(minWidth = 0))
                    // Подпись центрирована на отметке, но не вылезает за края.
                    val left = (x - p.width / 2).coerceIn(0, (width - p.width).coerceAtLeast(0))
                    layout(p.width, p.height) { p.place(left, 0) }
                },
            )
        }
    }
}

/** Шеврон «назад», нарисованный линией в цвет текста. */
@Composable
fun BackChevron(color: Color, modifier: Modifier = Modifier) {
    Canvas(modifier.size(18.dp)) {
        val s = size.minDimension
        drawLine(color, Offset(s * 0.62f, s * 0.2f), Offset(s * 0.3f, s * 0.5f), 1.5.dp.toPx(), StrokeCap.Round)
        drawLine(color, Offset(s * 0.3f, s * 0.5f), Offset(s * 0.62f, s * 0.8f), 1.5.dp.toPx(), StrokeCap.Round)
    }
}

/** «Играть» — треугольник, «стоп» — квадрат; линейный стиль, цвет текста. */
@Composable
fun PlayGlyph(playing: Boolean, color: Color, modifier: Modifier = Modifier) {
    Canvas(modifier.size(16.dp)) {
        val s = size.minDimension
        if (playing) {
            drawRect(color, Offset(s * 0.22f, s * 0.22f), Size(s * 0.56f, s * 0.56f))
        } else {
            val path = androidx.compose.ui.graphics.Path().apply {
                moveTo(s * 0.28f, s * 0.18f)
                lineTo(s * 0.82f, s * 0.5f)
                lineTo(s * 0.28f, s * 0.82f)
                close()
            }
            drawPath(path, color, style = androidx.compose.ui.graphics.drawscope.Stroke(width = 1.5.dp.toPx(), join = androidx.compose.ui.graphics.StrokeJoin.Round))
        }
    }
}
