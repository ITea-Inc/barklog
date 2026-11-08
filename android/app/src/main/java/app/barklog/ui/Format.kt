package app.barklog.ui

import uniffi.barklog_ffi.SoundKind
import uniffi.barklog_ffi.StartInfo
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneOffset
import java.util.Locale

/** Форматирование — то же, что в HTML-отчёте CLI (crates/barklog-cli/src/fmt.rs). */
object Fmt {
    private val MONTHS = arrayOf(
        "января", "февраля", "марта", "апреля", "мая", "июня",
        "июля", "августа", "сентября", "октября", "ноября", "декабря",
    )

    /** «1 ч 12 мин», «6 мин 12 с», «45 с», «< 1 с». */
    fun duration(ms: Long): String {
        if (ms in 1..999) return "< 1 с"
        val total = (ms + 500) / 1000
        val h = total / 3600
        val m = total / 60 % 60
        val s = total % 60
        return when {
            h == 0L && m == 0L -> "$s с"
            h == 0L && s == 0L -> "$m мин"
            h == 0L -> "$m мин $s с"
            m == 0L -> "$h ч"
            else -> "$h ч $m мин"
        }
    }

    fun duration(ms: ULong) = duration(ms.toLong())

    /** Счётчик ленты «02:14:30». */
    fun counter(ms: Long): String {
        val s = ms / 1000
        return String.format(Locale.ROOT, "%02d:%02d:%02d", s / 3600, s / 60 % 60, s % 60)
    }

    fun counter(ms: ULong) = counter(ms.toLong())

    /** «9,4 %». */
    fun percent(p: Float): String = String.format(Locale.ROOT, "%.1f %%", p).replace('.', ',')

    fun local(start: StartInfo, offsetMs: Long): OffsetDateTime =
        Instant.ofEpochMilli(start.unixMs + offsetMs)
            .atOffset(ZoneOffset.ofTotalSeconds(start.utcOffsetMin * 60))

    /** «09:44:05» на часах, если известно начало, иначе счётчик ленты. */
    fun clock(start: StartInfo?, offsetMs: Long): String =
        if (start == null) counter(offsetMs) else local(start, offsetMs).let {
            String.format(Locale.ROOT, "%02d:%02d:%02d", it.hour, it.minute, it.second)
        }

    fun hhmm(start: StartInfo, offsetMs: Long): String =
        local(start, offsetMs).let { String.format(Locale.ROOT, "%02d:%02d", it.hour, it.minute) }

    /** «7 ноября 2026», без года — если год текущий. */
    fun date(start: StartInfo, withYear: Boolean = true): String {
        val t = local(start, 0)
        val base = "${t.dayOfMonth} ${MONTHS[t.monthValue - 1]}"
        return if (withYear) "$base ${t.year}" else base
    }

    fun isCurrentYear(start: StartInfo): Boolean =
        local(start, 0).year == OffsetDateTime.now().year

    /** «2 ноября», с годом — «2 ноября 2025». */
    fun day(day: Int, month: Int, year: Int? = null): String =
        "$day ${MONTHS[month - 1]}" + (year?.let { " $it" } ?: "")

    /** «2.11» — подпись под столбцом. */
    fun dayShort(day: Int, month: Int): String = String.format(Locale.ROOT, "%d.%02d", day, month)

    /** 1 запись, 3 записи, 12 записей. */
    fun records(n: Int): String {
        val mod100 = n % 100
        val mod10 = n % 10
        val word = when {
            mod100 in 11..14 -> "записей"
            mod10 == 1 -> "запись"
            mod10 in 2..4 -> "записи"
            else -> "записей"
        }
        return "$n $word"
    }

    /** 1 эпизод, 3 эпизода, 12 эпизодов. */
    fun episodes(n: Int): String {
        val mod100 = n % 100
        val mod10 = n % 10
        val word = when {
            mod100 in 11..14 -> "эпизодов"
            mod10 == 1 -> "эпизод"
            mod10 in 2..4 -> "эпизода"
            else -> "эпизодов"
        }
        return "$n $word"
    }

    fun label(kind: SoundKind): String = when (kind) {
        SoundKind.BARK -> "Лай"
        SoundKind.WHINE -> "Скулёж"
        SoundKind.HOWL -> "Вой"
        SoundKind.GROWL -> "Рычание"
        SoundKind.OTHER -> "Другие звуки"
        SoundKind.SILENCE -> "Тишина"
    }

    /** Подписи оси на круглых значениях времени (как в отчёте CLI): не больше 8. */
    fun axisTicks(start: StartInfo?, totalMs: Long): List<Pair<Long, String>> {
        val min = 60_000L
        val step = listOf(1, 2, 5, 10, 15, 30, 60, 120).map { it * min }
            .firstOrNull { totalMs / it <= 7 } ?: (240 * min)
        val phase = start?.let { Math.floorMod(it.unixMs + it.utcOffsetMin * min, step) } ?: 0L
        val first = if (phase == 0L) 0L else step - phase
        return generateSequence(first) { it + step }.takeWhile { it <= totalMs }.map { t ->
            val label = when {
                start != null -> hhmm(start, t)
                totalMs >= 60 * min -> String.format(Locale.ROOT, "%d:%02d", t / (60 * min), t / min % 60)
                else -> "${t / min} мин"
            }
            t to label
        }.toList()
    }
}
