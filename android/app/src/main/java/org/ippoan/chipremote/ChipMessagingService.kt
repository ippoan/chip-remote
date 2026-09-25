package org.ippoan.chipremote

import android.os.Build
import android.util.Log
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

class ChipMessagingService : FirebaseMessagingService() {

    override fun onNewToken(token: String) {
        val client = Settings.client(this) ?: return
        scope.launch {
            try {
                client.registerDevice(token, Build.MODEL)
                Log.i(TAG, "re-registered device with new FCM token")
            } catch (e: Exception) {
                Log.w(TAG, "re-register failed", e)
            }
        }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        when (val ev = FcmEvent.parse(message.data)) {
            is FcmEvent.New -> Notifier.showChip(this, ev.chip)
            is FcmEvent.Result -> {
                // 通知が既にスワイプされていたら結果だけ出す
                val chip = Notifier.findChip(this, ev.taskId)
                    ?: ChipNotice(ev.taskId, title = "chip", tldr = "", cwd = "", host = "", located = true)
                if (ev.ok) Notifier.showDone(this, chip, ev.action)
                else Notifier.showError(this, chip, errorLabel(ev.error))
            }
            is FcmEvent.Cancel -> Notifier.cancel(this, ev.taskId)
            null -> Log.w(TAG, "ignored FCM data: type=${message.data["type"]}")
        }
    }

    companion object {
        private const val TAG = "ChipMessaging"
        private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    }
}
