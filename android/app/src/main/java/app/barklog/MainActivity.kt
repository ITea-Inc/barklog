package app.barklog

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.core.content.IntentCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.barklog.ui.BarkLogTheme
import app.barklog.ui.LiveScreen
import app.barklog.ui.ReportScreen
import app.barklog.ui.SessionsScreen

class MainActivity : ComponentActivity() {

    private val vm: MainViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (savedInstanceState == null) handleShare(intent)

        setContent {
            BarkLogTheme {
                var openId by rememberSaveable { mutableStateOf<Long?>(null) }
                var showLive by rememberSaveable { mutableStateOf(false) }
                val live by vm.live.collectAsStateWithLifecycle()

                // Запись сохранена — сразу открываем её отчёт.
                LaunchedEffect(live) {
                    val s = live
                    if (s is LiveState.Finished) {
                        showLive = false
                        openId = s.sessionId
                        vm.acknowledgeLive()
                        vm.refresh()
                    } else if (s is LiveState.Failed) {
                        showLive = false
                        vm.showError(s.message)
                        vm.acknowledgeLive()
                    }
                }

                val id = openId
                if (showLive) {
                    BackHandler { showLive = false }
                    LiveScreen(live, onBack = { showLive = false }, onStop = vm::stopRecording)
                } else if (id == null) {
                    SessionsScreen(vm, onOpen = { openId = it }, onOpenLive = { showLive = true })
                } else {
                    BackHandler { openId = null }
                    ReportScreen(
                        vm = vm,
                        id = id,
                        onBack = { openId = null },
                        onDeleted = { openId = null; vm.delete(id) },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleShare(intent)
    }

    private fun handleShare(intent: Intent?) {
        if (intent?.action != Intent.ACTION_SEND) return
        val uri = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java) ?: return
        vm.analyzeShared(uri)
    }

    companion object {
        /** Разрешение на уведомления нужно спрашивать с Android 13. */
        val NEEDS_NOTIFICATION_PERMISSION = Build.VERSION.SDK_INT >= 33
    }
}
