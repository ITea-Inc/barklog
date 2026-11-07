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
import androidx.compose.foundation.lazy.LazyColumn
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.barklog.LiveState
import uniffi.barklog_ffi.LiveSnapshot

/** Экран идущей записи. Он же — в этапе 3б — экран просмотра с другого телефона. */
@Composable
fun LiveScreen(state: LiveState, onBack: () -> Unit, onStop: () -> Unit) {
    val c = Bark.colors
    val snap = (state as? LiveState.Recording)?.snapshot

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
        }

        LazyColumn(
            Modifier.weight(1f),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item { Header(state, snap) }
            if (snap != null) {
                item { Stats(snap) }
                item {
                    var selected by remember { mutableStateOf<String?>(null) }
                    Panel(Modifier.fillMaxWidth()) {
                        PanelHeader("С начала записи", "Столбец — ${binLabel(snap.report.timelineBinMs.toLong())}")
                        Legend(DOG_KINDS)
                        Spacer(Modifier.height(16.dp))
                        Timeline(
                            snap.report.timeline,
                            snap.report.startedAt,
                            snap.report.durationMs.toLong().coerceAtLeast(1),
                            onSelect = { selected = it },
                        )
                        Spacer(Modifier.height(8.dp))
                        Text(
                            selected ?: "Анализ отстаёт от записи на несколько секунд",
                            style = Bark.type.mono.copy(color = if (selected != null) c.text else c.muted),
                        )
                    }
                }
            }
            item {
                Text(
                    "Оставьте телефон на зарядке рядом с местом, где обычно лежит собака. " +
                        "Экран можно выключить — запись продолжится. Звук целиком не сохраняется, " +
                        "только короткие фрагменты, где слышна собака",
                    style = Bark.type.caption,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        }

        val saving = state is LiveState.Saving
        Box(
            Modifier
                .padding(16.dp)
                .fillMaxWidth()
                .height(48.dp)
                .clip(RoundedCornerShape(10.dp))
                .background(if (saving) c.raised else c.primary)
                .clickable(enabled = state is LiveState.Recording, onClick = onStop),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                if (saving) "Сохраняю отчёт" else "Я вернулся",
                style = Bark.type.body.copy(color = if (saving) c.text2 else c.onPrimary),
            )
        }
    }
}

@Composable
private fun Header(state: LiveState, snap: LiveSnapshot?) {
    val c = Bark.colors
    Column(Modifier.padding(top = 8.dp, bottom = 12.dp)) {
        Text(
            when (state) {
                LiveState.Starting -> "Включаю микрофон"
                LiveState.Saving -> "Сохраняю"
                else -> "Идёт запись"
            },
            style = Bark.type.title,
        )
        Spacer(Modifier.height(12.dp))
        // Подпись продукта: время как на счётчике ленты.
        Text(
            Fmt.counter(snap?.elapsedMs?.toLong() ?: 0),
            style = Bark.type.monoLarge.copy(fontSize = 44.sp, lineHeight = 48.sp, color = c.text),
        )
        Spacer(Modifier.height(16.dp))
        if (snap != null) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Dot(if (snap.dogNow) c.bark else c.muted)
                Spacer(Modifier.width(8.dp))
                Text(status(snap), style = Bark.type.body)
            }
        }
    }
}

private fun status(s: LiveSnapshot): String {
    if (s.dogNow) return "Собака подаёт голос"
    val last = s.lastDogSoundMs ?: return "Тихо с начала записи"
    val ago = (s.analyzedMs.toLong() - last.toLong()).coerceAtLeast(0)
    return if (ago < 60_000) "Сейчас тихо" else "Тихо уже ${Fmt.duration(ago - ago % 60_000)}"
}

@Composable
private fun Stats(s: LiveSnapshot) {
    val r = s.report
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            StatCard("Голос собаки", Fmt.duration(r.dogMs), "${Fmt.percent(r.dogPercent)} записи", Modifier.weight(1f))
            StatCard("За 15 минут", Fmt.duration(s.recentDogMs), "голос собаки", Modifier.weight(1f))
        }
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            StatCard(
                "Эпизодов", r.episodesCount.toString(),
                r.longestEpisodeMs?.let { "самый длинный ${Fmt.duration(it)}" } ?: "пока не было",
                Modifier.weight(1f),
            )
            StatCard(
                "Первый звук",
                r.firstDogSoundMs?.let { Fmt.counter(it) } ?: "—",
                r.firstDogSoundMs?.let { "в ${Fmt.clock(r.startedAt, it.toLong())}" } ?: "пока не было",
                Modifier.weight(1f),
            )
        }
    }
}

private fun binLabel(ms: Long) = if (ms >= 60_000) "${ms / 60_000} мин" else "${ms / 1000} с"
