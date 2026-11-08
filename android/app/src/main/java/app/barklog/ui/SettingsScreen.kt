package app.barklog.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.barklog.MainViewModel
import uniffi.barklog_ffi.SensitivitySettings
import java.util.Locale
import kotlin.math.abs
import kotlin.math.roundToInt

/** Шаги ползунков пауз и минимального эпизода, мс: мелко в начале, крупно в конце. */
private val GAP_STEPS = listOf(0L, 1_000, 2_000, 3_000, 5_000, 10_000, 20_000, 30_000)
private val MIN_EPISODE_STEPS = listOf(0L, 500, 1_000, 1_500, 2_000, 3_000, 5_000, 10_000, 20_000, 30_000)

/** Порог «это собака» слева направо — от строгого к чувствительному; рабочая зона мельче. */
private val DOG_MIN_STEPS = listOf(0.9f, 0.7f, 0.5f, 0.4f, 0.3f, 0.25f, 0.2f, 0.15f, 0.12f, 0.1f, 0.07f, 0.05f)

/** Чувствительность: порог «это собака» и правила сборки эпизодов. */
@Composable
fun SettingsScreen(vm: MainViewModel, onBack: () -> Unit) {
    val c = Bark.colors
    val saved by vm.sensitivity.collectAsStateWithLifecycle()
    val applying by vm.applying.collectAsStateWithLifecycle()
    val personal by vm.personal.collectAsStateWithLifecycle()
    val limits = vm.sensitivityLimits

    Column(
        Modifier
            .fillMaxSize()
            .background(c.bg)
            .safeDrawingPadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Row(
            Modifier.clip(RoundedCornerShape(8.dp)).clickable(onClick = onBack).padding(vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            BackChevron(c.text)
            Spacer(Modifier.width(4.dp))
            Text("Записи", style = Bark.type.body)
        }
        Spacer(Modifier.height(16.dp))
        Text("Настройки", style = Bark.type.title)
        Spacer(Modifier.height(8.dp))
        Text(
            "Если приложение пропускает звуки собаки или принимает за неё посторонний шум, " +
                "поменяйте чувствительность. Все записи пересчитаются без повторного анализа",
            style = Bark.type.subtitle,
        )
        Spacer(Modifier.height(24.dp))

        val current = saved ?: return@Column
        // Черновик живёт до «Применить»: пересчёт всех записей небыстрый, на каждое движение — не годится.
        var dogMin by rememberSaveable(current) { mutableStateOf(current.dogMin) }
        var gapMs by rememberSaveable(current) { mutableStateOf(current.mergeGapMs.toLong()) }
        var minMs by rememberSaveable(current) { mutableStateOf(current.minEpisodeMs.toLong()) }
        val draft = SensitivitySettings(dogMin, gapMs.toULong(), minMs.toULong())

        Panel(Modifier.fillMaxWidth()) {
            PanelHeader(
                "Чувствительность",
                "Чем выше, тем больше звуков считается собакой — и тем чаще ошибки",
            )
            StepSlider(DOG_MIN_STEPS.filter { it in limits.dogMinLow..limits.dogMinHigh }, dogMin, onChange = { dogMin = it })
            Spacer(Modifier.height(4.dp))
            Row {
                Text("Строже", style = Bark.type.caption, modifier = Modifier.weight(1f))
                Text("Чувствительнее", style = Bark.type.caption)
            }
            Spacer(Modifier.height(12.dp))
            Text(
                "Порог уверенности ${String.format(Locale.ROOT, "%.0f %%", dogMin * 100)}",
                style = Bark.type.body.copy(color = c.text2),
            )
            if (personal?.trained == true) {
                Spacer(Modifier.height(12.dp))
                Text(
                    "Сейчас звуки разбирает личная модель — на её решения порог не влияет. " +
                        "Он снова заработает, если сбросить модель",
                    style = Bark.type.caption,
                )
            }
        }
        Spacer(Modifier.height(12.dp))

        Panel(Modifier.fillMaxWidth()) {
            PanelHeader("Паузы внутри эпизода", "Более короткая пауза не разрывает эпизод на два")
            StepSlider(GAP_STEPS, gapMs, onChange = { gapMs = it })
            Spacer(Modifier.height(12.dp))
            Text(
                if (gapMs == 0L) "Не склеивать" else "Склеивать паузы до ${seconds(gapMs)}",
                style = Bark.type.body.copy(color = c.text2),
            )
        }
        Spacer(Modifier.height(12.dp))

        Panel(Modifier.fillMaxWidth()) {
            PanelHeader(
                "Самый короткий эпизод",
                "Эпизоды короче не считаются — так одиночные ложные срабатывания не попадут в отчёт",
            )
            StepSlider(MIN_EPISODE_STEPS, minMs, onChange = { minMs = it })
            Spacer(Modifier.height(12.dp))
            Text(
                if (minMs == 0L) "Считать все" else "Не короче ${seconds(minMs)}",
                style = Bark.type.body.copy(color = c.text2),
            )
        }
        Spacer(Modifier.height(24.dp))

        val changed = !draft.sameAs(current)
        val isDefault = draft.sameAs(limits.defaults)
        if (applying) {
            Text("Пересчитываю записи", style = Bark.type.body.copy(color = c.text2))
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(16.dp), verticalAlignment = Alignment.CenterVertically) {
                if (changed) PrimaryAction("Применить", onClick = { vm.setSensitivity(draft) })
                if (!isDefault) {
                    TextLink("Как было по умолчанию", c.text2) {
                        val d = limits.defaults
                        dogMin = d.dogMin
                        gapMs = d.mergeGapMs.toLong()
                        minMs = d.minEpisodeMs.toLong()
                    }
                }
            }
        }
        Spacer(Modifier.height(24.dp))
    }
}

/** Ползунок по заданным значениям; текущее значение притягивается к ближайшему шагу. */
@Composable
private fun <T : Number> StepSlider(values: List<T>, value: T, onChange: (T) -> Unit) {
    val index = values.indices.minBy { abs(values[it].toDouble() - value.toDouble()) }
    Slider(
        value = index.toFloat(),
        onValueChange = { onChange(values[it.roundToInt().coerceIn(values.indices)]) },
        valueRange = 0f..values.lastIndex.toFloat(),
        steps = values.size - 2,
        colors = sliderColors(),
    )
}

@Composable
private fun sliderColors() = Bark.colors.let { c ->
    SliderDefaults.colors(
        thumbColor = c.bark,
        activeTrackColor = c.bark,
        inactiveTrackColor = c.raised,
        activeTickColor = c.bark,
        inactiveTickColor = c.line,
    )
}

/** «0,5 с», «3 с». */
private fun seconds(ms: Long): String =
    if (ms % 1000 == 0L) "${ms / 1000} с" else String.format(Locale.ROOT, "%.1f с", ms / 1000.0).replace('.', ',')

private fun SensitivitySettings.sameAs(o: SensitivitySettings) =
    abs(dogMin - o.dogMin) < 0.005f && mergeGapMs == o.mergeGapMs && minEpisodeMs == o.minEpisodeMs
