package app.barklog

import android.content.Context
import android.content.Intent
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Typeface
import android.graphics.pdf.PdfDocument
import android.text.Layout
import android.text.StaticLayout
import android.text.TextPaint
import androidx.core.content.FileProvider
import androidx.core.content.res.ResourcesCompat
import app.barklog.ui.DOG_KINDS
import app.barklog.ui.Fmt
import uniffi.barklog_ffi.LabelKind
import uniffi.barklog_ffi.SessionReport
import java.io.File

/** Выгрузка отчётов: PDF для кинолога или ветеринара и таблицы CSV. */
object Export {

    /** Отправляет файл из кэша через системное «Поделиться». */
    fun share(context: Context, file: File, mime: String, title: String) {
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
        val send = Intent(Intent.ACTION_SEND)
            .setType(mime)
            .putExtra(Intent.EXTRA_STREAM, uri)
            .putExtra(Intent.EXTRA_SUBJECT, title)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        context.startActivity(Intent.createChooser(send, title).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    fun writeCsv(context: Context, name: String, csv: String): File =
        exportFile(context, "$name.csv").apply { writeText(csv, Charsets.UTF_8) }

    /** Имя файла по дате записи: «barklog-2026-11-07». */
    fun baseName(r: SessionReport): String {
        val start = r.startedAt ?: return "barklog-${r.id}"
        val t = Fmt.local(start, 0)
        return String.format(java.util.Locale.ROOT, "barklog-%04d-%02d-%02d-%02d%02d", t.year, t.monthValue, t.dayOfMonth, t.hour, t.minute)
    }

    private fun exportFile(context: Context, name: String): File {
        val dir = context.cacheDir.resolve("exports").apply { mkdirs() }
        // Старые выгрузки не копим: их уже отправили.
        dir.listFiles()?.filter { System.currentTimeMillis() - it.lastModified() > 86_400_000 }?.forEach { it.delete() }
        return dir.resolve(name)
    }

    /** Отчёт о записи на страницах A4 — светлый при любой теме телефона, чтобы печатался. */
    fun writePdf(context: Context, r: SessionReport): File {
        val file = exportFile(context, "${baseName(r)}.pdf")
        val pdf = Pdf(context)
        pdf.report(r)
        file.outputStream().use { pdf.doc.writeTo(it) }
        pdf.doc.close()
        return file
    }

    private class Pdf(context: Context) {
        val doc = PdfDocument()
        private val sans: Typeface = ResourcesCompat.getFont(context, R.font.golos_text) ?: Typeface.SANS_SERIF
        private val mono: Typeface = ResourcesCompat.getFont(context, R.font.jetbrains_mono) ?: Typeface.MONOSPACE

        private val text = 0xFF171717.toInt()
        private val text2 = 0xFF525252.toInt()
        private val muted = 0xFF737373.toInt()
        private val line = 0xFFE5E5E5.toInt()
        private val bark = 0xFFC27F22.toInt()

        private var page: PdfDocument.Page? = null
        private lateinit var canvas: Canvas
        private var pageNo = 0
        private var y = 0f

        fun paint(size: Float, color: Int = text, face: Typeface = sans, medium: Boolean = false) = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
            textSize = size
            this.color = color
            // Вес 500 у вариативного шрифта задаётся только с Android 9; раньше — обычный.
            typeface = if (medium && android.os.Build.VERSION.SDK_INT >= 28) Typeface.create(face, 500, false) else face
        }

        private fun newPage() {
            page?.let { finishPage(it) }
            pageNo++
            val p = doc.startPage(PdfDocument.PageInfo.Builder(W, H, pageNo).create())
            page = p
            canvas = p.canvas
            y = M
        }

        private fun finishPage(p: PdfDocument.Page) {
            val foot = paint(7.5f, muted)
            p.canvas.drawText("BarkLog · анализ на телефоне, модель YAMNet · стр. $pageNo", M, H - 24f, foot)
            doc.finishPage(p)
        }

        /** Переносит на новую страницу, если `h` не помещается. */
        private fun need(h: Float) {
            if (page == null || y + h > H - 48f) newPage()
        }

        private fun para(s: String, p: TextPaint, width: Float = W - 2 * M, x: Float = M, after: Float = 0f) {
            val layout = StaticLayout.Builder.obtain(s, 0, s.length, p, width.toInt())
                .setAlignment(Layout.Alignment.ALIGN_NORMAL)
                .setLineSpacing(0f, 1.15f)
                .build()
            need(layout.height.toFloat())
            canvas.save()
            canvas.translate(x, y)
            layout.draw(canvas)
            canvas.restore()
            y += layout.height + after
        }

        private fun heading(s: String, note: String? = null) {
            need(60f)
            y += 18f
            para(s, paint(12f, medium = true), after = 2f)
            if (note != null) para(note, paint(8.5f, muted), after = 4f)
            y += 6f
        }

        private fun rule() {
            canvas.drawLine(M, y, W - M, y, Paint().apply { color = line; strokeWidth = 0.75f })
        }

        /** Строка таблицы: колонки с правыми краями `rights` (последняя — по правому полю). */
        private fun tableRow(cells: List<String>, lefts: List<Float>, p: TextPaint, rightAligned: Set<Int> = emptySet()) {
            val h = 18f
            need(h)
            val base = y + 12.5f
            cells.forEachIndexed { i, s ->
                val right = lefts.getOrNull(i + 1)?.minus(8f) ?: (W - M)
                if (i in rightAligned) canvas.drawText(s, right - p.measureText(s), base, p)
                else canvas.drawText(ellipsize(s, p, right - lefts[i]), lefts[i], base, p)
            }
            y += h
        }

        private fun ellipsize(s: String, p: TextPaint, width: Float): String {
            if (p.measureText(s) <= width) return s
            var end = s.length
            while (end > 0 && p.measureText(s, 0, end) + p.measureText("…") > width) end--
            return s.substring(0, end) + "…"
        }

        fun report(r: SessionReport) {
            newPage()
            val start = r.startedAt
            para("Отчёт о записи · BarkLog", paint(8.5f, muted), after = 6f)
            para(if (start != null) Fmt.date(start) else r.sourceName, paint(22f, medium = true), after = 2f)
            if (start != null) {
                para("${Fmt.hhmm(start, 0)}–${Fmt.hhmm(start, r.durationMs.toLong())}", paint(12f, text2, mono), after = 10f)
            }
            val summary = if (r.dogMs == 0uL) {
                "Запись ${Fmt.duration(r.durationMs)}. Собаку не слышно."
            } else {
                "Запись ${Fmt.duration(r.durationMs)}. Собака подавала голос ${Fmt.duration(r.dogMs)} — " +
                    "${Fmt.percent(r.dogPercent)} времени, ${Fmt.episodes(r.episodesCount.toInt())}."
            }
            para(summary, paint(11f, text2), after = 6f)
            r.note?.let { para("Заметка хозяина: $it", paint(10f, text), after = 4f) }

            figures(r)
            timeline(r)
            shares(r)
            hours(r)
            episodes(r)
            page?.let { finishPage(it) }
            page = null
        }

        private fun figures(r: SessionReport) {
            y += 14f
            need(52f)
            val start = r.startedAt
            val cells = listOf(
                Triple("Голос собаки", Fmt.duration(r.dogMs), "${Fmt.percent(r.dogPercent)} записи"),
                Triple("Эпизодов", r.episodesCount.toString(), r.longestEpisodeMs?.let { "самый длинный ${Fmt.duration(it)}" } ?: "не было"),
                Triple("Первый звук", r.firstDogSoundMs?.let { Fmt.counter(it) } ?: "—", r.firstDogSoundMs?.let { "в ${Fmt.clock(start, it.toLong())}" } ?: "собаку не слышно"),
                Triple("Самый долгий покой", Fmt.duration(r.longestCalmMs), "без звуков собаки"),
            )
            val w = (W - 2 * M - 3 * 10f) / 4
            val label = paint(8f, muted)
            val value = paint(14f, medium = true)
            val note = paint(7.5f, muted)
            cells.forEachIndexed { i, (l, v, n) ->
                val x = M + i * (w + 10f)
                val box = RectF(x, y, x + w, y + 52f)
                canvas.drawRoundRect(box, 6f, 6f, Paint(Paint.ANTI_ALIAS_FLAG).apply { color = line; style = Paint.Style.STROKE; strokeWidth = 0.75f })
                canvas.drawText(ellipsize(l, label, w - 16f), x + 8f, y + 14f, label)
                canvas.drawText(ellipsize(v, value, w - 16f), x + 8f, y + 33f, value)
                canvas.drawText(ellipsize(n, note, w - 16f), x + 8f, y + 45f, note)
            }
            y += 52f
        }

        /** Одна серия: сколько звучала собака в каждом столбце времени. */
        private fun timeline(r: SessionReport) {
            val bin = r.timelineBinMs.toLong()
            val unit = if (bin >= 60_000) "${bin / 60_000} мин" else "${bin / 1000} с"
            heading("Когда звучала собака", "Столбец — $unit, высота — какую часть столбца звучала собака")
            val h = 90f
            need(h + 16f)
            val fractions = r.timeline.map { b ->
                val len = (b.endMs - b.startMs).toFloat().coerceAtLeast(1f)
                DOG_KINDS.sumOf { b.classMs[it.ordinal] }.toFloat() / len
            }
            val max = fractions.maxOrNull() ?: 0f
            val scale = listOf(0.1f, 0.25f, 0.5f, 1f).firstOrNull { max <= it } ?: 1f
            canvas.drawText("шкала до ${(scale * 100).toInt()} %", M, y + 8f, paint(7.5f, muted))
            val top = y + 14f
            val base = top + h
            val grid = Paint().apply { color = line; strokeWidth = 0.75f }
            canvas.drawLine(M, top, W - M, top, grid)
            canvas.drawLine(M, base, W - M, base, grid)
            if (fractions.isNotEmpty()) {
                val slot = (W - 2 * M) / fractions.size
                val bw = (slot * 0.7f).coerceAtLeast(0.5f)
                val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = bark }
                fractions.forEachIndexed { i, f ->
                    val bh = (f / scale).coerceAtMost(1f) * h
                    if (bh > 0f) canvas.drawRect(M + i * slot + (slot - bw) / 2, base - bh, M + i * slot + (slot + bw) / 2, base, fill)
                }
            }
            val axis = paint(7.5f, muted, mono)
            val start = r.startedAt
            canvas.drawText(Fmt.clock(start, 0).take(5), M, base + 11f, axis)
            val end = Fmt.clock(start, r.durationMs.toLong()).take(5)
            canvas.drawText(end, W - M - axis.measureText(end), base + 11f, axis)
            y = base + 16f
        }

        private fun shares(r: SessionReport) {
            heading("Из чего состояла запись")
            val lefts = listOf(M, M + 260f, M + 380f)
            tableRow(listOf("Звук", "Время", "От записи"), lefts, paint(8f, muted), setOf(1, 2))
            val body = paint(9.5f)
            r.classes.forEach { c ->
                rule()
                tableRow(listOf(Fmt.label(c.kind), Fmt.duration(c.timeMs), Fmt.percent(c.percent)), lefts, body, setOf(1, 2))
            }
            if (r.dogMs > 0uL) {
                y += 6f
                val dog = r.classes.filter { (it.dogPercent ?: 0f) > 0f }
                    .joinToString(" · ") { "${Fmt.label(it.kind).lowercase()} ${Fmt.percent(it.dogPercent!!)}" }
                para("Среди голоса собаки: $dog", paint(8.5f, text2))
            }
        }

        private fun hours(r: SessionReport) {
            if (r.hourly.size < 2) return
            val known = r.startedAt != null
            heading("По часам", if (known) "Сколько звучала собака в каждый час" else "Часы от начала записи")
            val lefts = listOf(M, M + 80f)
            val body = paint(9.5f)
            r.hourly.forEachIndexed { i, b ->
                if (i > 0) rule()
                val dog = DOG_KINDS.sumOf { b.classMs[it.ordinal] }
                tableRow(
                    listOf(
                        if (known) String.format(java.util.Locale.ROOT, "%02d:00", b.hour.toInt()) else "${b.hour.toInt() + 1}-й час",
                        if (dog == 0uL) "—" else Fmt.duration(dog),
                    ),
                    lefts, body,
                )
            }
        }

        private fun episodes(r: SessionReport) {
            heading(
                "Эпизоды",
                "${Fmt.episodes(r.episodes.size)}; паузы до ${r.mergeGapMs.toLong() / 1000} с не разрывают эпизод. " +
                    "«Отметка» — что указал хозяин, если поправлял",
            )
            if (r.episodes.isEmpty()) {
                para("За запись собака не подавала голос", paint(9.5f, text2))
                return
            }
            val lefts = listOf(M, M + 36f, M + 130f, M + 220f, M + 320f)
            val head = paint(8f, muted)
            tableRow(listOf("№", "Начало", "Длительность", "Что", "Отметка"), lefts, head)
            val body = paint(9.5f)
            val time = paint(9f, text, mono)
            r.episodes.forEachIndexed { i, e ->
                if (y + 18f > H - 48f) {
                    newPage()
                    tableRow(listOf("№", "Начало", "Длительность", "Что", "Отметка"), lefts, head)
                }
                rule()
                need(18f)
                val base = y + 12.5f
                canvas.drawText("${i + 1}", lefts[0], base, body)
                canvas.drawText(Fmt.clock(r.startedAt, e.startMs.toLong()), lefts[1], base, time)
                canvas.drawText(Fmt.duration((e.endMs - e.startMs).toLong()), lefts[2], base, body)
                canvas.drawText(Fmt.label(e.kind).lowercase(), lefts[3], base, body)
                e.label?.let { canvas.drawText(labelText(it), lefts[4], base, paint(9.5f, text2)) }
                y += 18f
            }
        }

        private fun labelText(l: LabelKind) = when (l) {
            LabelKind.BARK -> "лай"
            LabelKind.WHINE -> "скулёж"
            LabelKind.HOWL -> "вой"
            LabelKind.GROWL -> "рычание"
            LabelKind.NOT_DOG -> "не собака"
        }

        companion object {
            /** A4 в пунктах. */
            const val W = 595
            const val H = 842
            const val M = 48f
        }
    }
}
