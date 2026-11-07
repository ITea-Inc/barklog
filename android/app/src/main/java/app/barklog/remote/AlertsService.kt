package app.barklog.remote

import android.app.PendingIntent
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import app.barklog.BarkLogApp
import app.barklog.MainActivity
import app.barklog.R
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * Уведомления «собака лает уже N минут» от Cloud Function (firebase/functions).
 * Когда приложение свёрнуто, система показывает их сама; здесь — показ, когда оно открыто,
 * и обновление токена устройства.
 */
class AlertsService : FirebaseMessagingService() {

    override fun onNewToken(token: String) {
        (application as BarkLogApp).remote.registerDevice(token)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val n = message.notification ?: return
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, BarkLogApp.CHANNEL_ALERTS)
            .setSmallIcon(R.drawable.ic_stat_analysis)
            .setContentTitle(n.title)
            .setContentText(n.body)
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        runCatching { NotificationManagerCompat.from(this).notify(ALERT_ID, notification) }
    }

    private companion object {
        const val ALERT_ID = 3
    }
}
