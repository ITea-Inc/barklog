package app.barklog.remote

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.os.Build
import android.util.Log
import androidx.credentials.CredentialManager
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import app.barklog.BuildConfig
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import com.google.firebase.auth.FirebaseAuth
import com.google.firebase.auth.GoogleAuthProvider
import com.google.firebase.firestore.FieldValue
import com.google.firebase.firestore.FirebaseFirestore
import com.google.firebase.firestore.SetOptions
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.tasks.await
import uniffi.barklog_ffi.LiveSnapshot
import uniffi.barklog_ffi.parseLiveSnapshot
import java.util.UUID

/** Запись, идущая на каком-то телефоне аккаунта (может быть и этим). */
data class RemoteLive(
    val deviceId: String,
    val deviceName: String,
    val recording: Boolean,
    /** Когда снимок записан на сервер (по часам сервера), мс. */
    val updatedAtMs: Long?,
    val snapshot: LiveSnapshot?,
    val isThisDevice: Boolean,
)

/**
 * Удалённый просмотр через Firebase: записывающий телефон публикует снимок в
 * `users/{uid}/live/state`, остальные телефоны аккаунта подписаны на этот документ.
 * Звук никуда не отправляется — только цифры и таймлайн (см. LiveSnapshot в ядре).
 */
class RemoteSync private constructor(
    private val context: Context,
    private val app: FirebaseApp?,
    /** Работаем с локальным Firebase Emulator Suite (отладка). */
    val emulator: Boolean,
) {
    val available: Boolean get() = app != null

    private val auth: FirebaseAuth? = app?.let { FirebaseAuth.getInstance(it) }
    private val db: FirebaseFirestore? = app?.let { FirebaseFirestore.getInstance(it) }

    private val _user = MutableStateFlow(auth?.currentUser?.let { Account(it.uid, it.email ?: it.displayName ?: "") })
    val user: StateFlow<Account?> = _user.asStateFlow()

    data class Account(val uid: String, val email: String)

    init {
        auth?.addAuthStateListener { a ->
            _user.value = a.currentUser?.let { Account(it.uid, it.email ?: it.displayName ?: "") }
            if (a.currentUser != null) registerDevice()
        }
    }

    val deviceId: String by lazy {
        val prefs = context.getSharedPreferences("remote", Context.MODE_PRIVATE)
        prefs.getString("device_id", null) ?: UUID.randomUUID().toString().also {
            prefs.edit().putString("device_id", it).apply()
        }
    }

    private val deviceName: String = "${Build.MANUFACTURER} ${Build.MODEL}".trim()

    /** Вход через Google. На эмуляторе Firebase — тестовый аккаунт без настоящего Google. */
    suspend fun signIn(activity: Activity) {
        val auth = auth ?: return
        val credential = if (emulator) {
            // Эмулятор Auth принимает неподписанный токен в виде JSON.
            GoogleAuthProvider.getCredential(
                """{"sub":"barklog-test","email":"test@barklog.local","email_verified":true}""",
                null,
            )
        } else {
            val option = GetSignInWithGoogleOption.Builder(webClientId()).build()
            val request = GetCredentialRequest.Builder().addCredentialOption(option).build()
            val result = CredentialManager.create(activity).getCredential(activity, request)
            val cred = result.credential
            check(cred is CustomCredential && cred.type == GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL) {
                "Неожиданный тип учётных данных"
            }
            GoogleAuthProvider.getCredential(GoogleIdTokenCredential.createFrom(cred.data).idToken, null)
        }
        auth.signInWithCredential(credential).await()
    }

    fun signOut() {
        auth?.signOut()
    }

    /** OAuth client id генерирует плагин google-services из google-services.json. */
    @SuppressLint("DiscouragedApi")
    private fun webClientId(): String {
        val id = context.resources.getIdentifier("default_web_client_id", "string", context.packageName)
        check(id != 0) { "В google-services.json нет OAuth-клиента — включите вход через Google в Firebase" }
        return context.getString(id)
    }

    private fun liveDoc(uid: String) = db!!.document("users/$uid/live/state")

    /** Публикует состояние записи этого телефона. Без входа — ничего не делает. */
    fun publish(recording: Boolean, startedAtMs: Long, snapshotJson: String?, snapshot: LiveSnapshot?) {
        val uid = _user.value?.uid ?: return
        val data = buildMap<String, Any?> {
            put("deviceId", deviceId)
            put("deviceName", deviceName)
            put("status", if (recording) "recording" else "finished")
            put("startedAtMs", startedAtMs)
            put("updatedAt", FieldValue.serverTimestamp())
            if (snapshotJson != null) put("snapshot", snapshotJson)
            if (snapshot != null) {
                // Отдельные поля — чтобы функция уведомлений не разбирала JSON.
                put("dogNow", snapshot.dogNow)
                put("activeBoutStartMs", snapshot.activeBoutStartMs?.toLong())
                put("activeBoutMs", snapshot.activeBoutMs?.toLong())
            }
        }
        // Без сети Firestore поставит запись в очередь и отправит позже.
        liveDoc(uid).set(data, SetOptions.merge())
            .addOnFailureListener { Log.w(TAG, "publish failed", it) }
    }

    /** Запись на телефонах аккаунта — обновляется в реальном времени. */
    fun observe(): Flow<RemoteLive?> {
        val uid = _user.value?.uid ?: return flowOf(null)
        return callbackFlow {
            val reg = liveDoc(uid).addSnapshotListener { doc, err ->
                if (err != null) {
                    Log.w(TAG, "observe failed", err)
                    return@addSnapshotListener
                }
                if (doc == null || !doc.exists()) {
                    trySend(null)
                    return@addSnapshotListener
                }
                val snapshot = doc.getString("snapshot")?.let { json ->
                    runCatching { parseLiveSnapshot(json) }.getOrNull()
                }
                val id = doc.getString("deviceId").orEmpty()
                trySend(
                    RemoteLive(
                        deviceId = id,
                        deviceName = doc.getString("deviceName").orEmpty(),
                        recording = doc.getString("status") == "recording",
                        updatedAtMs = doc.getTimestamp("updatedAt")?.toDate()?.time,
                        snapshot = snapshot,
                        isThisDevice = id == deviceId,
                    ),
                )
            }
            awaitClose { reg.remove() }
        }
    }

    /** Регистрирует устройство и его FCM-токен для уведомлений «собака лает N минут». */
    fun registerDevice(token: String? = null) {
        val uid = _user.value?.uid ?: return
        val doc = db!!.document("users/$uid/devices/$deviceId")
        val base = mapOf("name" to deviceName, "updatedAt" to FieldValue.serverTimestamp())
        if (token != null) {
            doc.set(base + ("fcmToken" to token), SetOptions.merge())
            return
        }
        doc.set(base, SetOptions.merge())
        if (!emulator) {
            runCatching {
                FirebaseMessaging.getInstance().token.addOnSuccessListener { t ->
                    doc.set(mapOf("fcmToken" to t), SetOptions.merge())
                }
            }
        }
    }

    companion object {
        private const val TAG = "BarkLog"
        private const val DEMO_PROJECT = "demo-barklog"

        /**
         * В отладке с -Pbarklog.firebaseEmulator=<host> — локальный Firebase Emulator Suite.
         * Иначе настоящий проект, если есть google-services.json (FirebaseApp создан плагином).
         * Иначе удалённый просмотр недоступен.
         */
        fun create(context: Context): RemoteSync {
            val existing = FirebaseApp.getApps(context).firstOrNull()
            val host = BuildConfig.FIREBASE_EMULATOR_HOST
            if (host.isBlank()) return RemoteSync(context, existing, emulator = false)

            // Отладка с эмулятором — даже если google-services.json есть: всё уходит локально.
            val app = existing ?: FirebaseApp.initializeApp(
                context,
                FirebaseOptions.Builder()
                    .setProjectId(DEMO_PROJECT)
                    .setApplicationId("1:000000000000:android:0000000000000000")
                    .setApiKey("demo-api-key")
                    .build(),
            )
            FirebaseAuth.getInstance(app).useEmulator(host, 9099)
            FirebaseFirestore.getInstance(app).useEmulator(host, 8080)
            return RemoteSync(context, app, emulator = true)
        }
    }
}
