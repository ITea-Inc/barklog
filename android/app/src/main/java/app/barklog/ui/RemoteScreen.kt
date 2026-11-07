package app.barklog.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.barklog.LiveState
import app.barklog.MainViewModel
import app.barklog.remote.RemoteLive
import kotlinx.coroutines.delay

/** Снимок старше этого — считаем, что записывающий телефон потерял связь. */
private const val STALE_MS = 2 * 60_000L

/** Просмотр записи, идущей на другом телефоне аккаунта. */
@Composable
fun RemoteScreen(vm: MainViewModel, onBack: () -> Unit) {
    val remote by vm.remoteLive.collectAsStateWithLifecycle()
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1_000)
            now = System.currentTimeMillis()
        }
    }

    val r = remote
    val snap = r?.snapshot
    if (r == null || snap == null) {
        Column(Modifier.fillMaxSize().background(Bark.colors.bg).safeDrawingPadding().padding(16.dp)) {
            Text("Удалённый просмотр", style = Bark.type.title)
            Spacer(Modifier.height(12.dp))
            Text("Записей на телефонах этого аккаунта пока не было", style = Bark.type.subtitle)
        }
        return
    }

    val ago = r.updatedAtMs?.let { (now - it).coerceAtLeast(0) }
    val stale = r.recording && (ago == null || ago > STALE_MS)
    // Между снимками счётчик досчитывает сам, пока связь есть.
    val elapsed = snap.elapsedMs.toLong() + if (r.recording && !stale && ago != null) ago else 0
    LiveScreen(
        state = LiveState.Recording(snap),
        onBack = onBack,
        onStop = {},
        readOnly = true,
        title = if (r.recording) "Дома идёт запись" else "Запись закончилась",
        note = note(r, ago, stale),
        elapsedMs = elapsed,
    )
}

private fun note(r: RemoteLive, ago: Long?, stale: Boolean): String {
    val device = if (r.isThisDevice) "этот телефон" else r.deviceName
    val fresh = when {
        ago == null -> "ждём первые данные"
        stale -> "нет связи ${Fmt.duration(ago - ago % 60_000)}"
        ago < 10_000 -> "только что"
        else -> "обновлено ${Fmt.duration(ago - ago % 1_000)} назад"
    }
    return "$device · $fresh"
}
