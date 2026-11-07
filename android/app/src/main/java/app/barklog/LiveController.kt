package app.barklog

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import uniffi.barklog_ffi.LiveSnapshot

/** Состояние живой записи — общее для сервиса и экранов. */
sealed interface LiveState {
    data object Idle : LiveState
    data object Starting : LiveState
    data class Recording(val snapshot: LiveSnapshot?) : LiveState
    data object Saving : LiveState
    data class Finished(val sessionId: Long) : LiveState
    data class Failed(val message: String) : LiveState
}

class LiveController {
    private val _state = MutableStateFlow<LiveState>(LiveState.Idle)
    val state: StateFlow<LiveState> = _state.asStateFlow()

    internal fun set(state: LiveState) {
        _state.value = state
    }

    /** Новый снимок — только пока запись идёт (запоздалый снимок не перетрёт «Сохраняю»). */
    internal fun publish(snapshot: LiveSnapshot) {
        _state.update { if (it is LiveState.Recording) LiveState.Recording(snapshot) else it }
    }

    /** Экран показал результат — возвращаемся в покой. */
    fun acknowledge() {
        val s = _state.value
        if (s is LiveState.Finished || s is LiveState.Failed) _state.value = LiveState.Idle
    }

    val isActive: Boolean
        get() = _state.value.let { it is LiveState.Starting || it is LiveState.Recording || it is LiveState.Saving }
}
