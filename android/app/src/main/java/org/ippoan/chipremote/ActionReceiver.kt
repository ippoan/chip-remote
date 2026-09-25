package org.ippoan.chipremote

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/** 通知の「開始」「非表示」→ POST /v1/chips/:task_id/action。 */
class ActionReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != INTENT_ACTION) return
        val chip = intent.extras?.let { Notifier.fromBundle(it) } ?: return
        val action = intent.getStringExtra(EXTRA_ACTION) ?: return
        val ctx = context.applicationContext

        val client = Settings.client(ctx)
        if (client == null) {
            Notifier.showError(ctx, chip, "アプリで Worker URL と token を設定してください")
            return
        }

        Notifier.showSending(ctx, chip)
        val pending = goAsync()
        scope.launch {
            try {
                when (val r = client.postAction(chip.taskId, action)) {
                    ActionOutcome.Accepted -> Notifier.showAcceptedIfStillSending(ctx, chip)
                    ActionOutcome.AgentOffline -> Notifier.showError(ctx, chip, errorLabel("agent_offline"))
                    ActionOutcome.ChipClosed -> Notifier.cancel(ctx, chip.taskId)
                    is ActionOutcome.Failed -> Notifier.showError(ctx, chip, r.message)
                }
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        const val INTENT_ACTION = "org.ippoan.chipremote.CHIP_ACTION"
        const val EXTRA_ACTION = "action"
        const val ACTION_START = "start"
        const val ACTION_DISMISS = "dismiss"

        private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    }
}
