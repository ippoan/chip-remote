package org.ippoan.chipremote

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.core.app.NotificationCompat

/**
 * chip 通知の組み立て。1 chip = 1 通知 (ID = task_id.hashCode())。
 * 状態が変わるたびに同じ ID で出し直して内容を差し替える。
 * chip の中身は通知 extras にも入れておき、FCM chip_result で更新するときに取り出す。
 */
object Notifier {
    const val CHANNEL_ID = "chips"

    private const val EXTRA_TASK_ID = "chip.task_id"
    private const val EXTRA_TITLE = "chip.title"
    private const val EXTRA_TLDR = "chip.tldr"
    private const val EXTRA_CWD = "chip.cwd"
    private const val EXTRA_HOST = "chip.host"
    private const val EXTRA_LOCATED = "chip.located"
    /** 通知が今どの段階を表示しているか。202 と FCM chip_result の到着順の逆転を見分けるのに使う。 */
    private const val EXTRA_STAGE = "chip.stage"
    private const val STAGE_CHIP = "chip"
    private const val STAGE_SENDING = "sending"
    private const val STAGE_ACCEPTED = "accepted"
    private const val STAGE_DONE = "done"
    private const val STAGE_ERROR = "error"

    private const val NOT_LOCATED_SUFFIX = "(画面で chip を確認できていません)"
    private const val DONE_TIMEOUT_MS = 5_000L

    fun ensureChannel(ctx: Context) {
        val channel = NotificationChannel(CHANNEL_ID, "chip", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Claude の推奨タスク (chip)"
        }
        manager(ctx).createNotificationChannel(channel)
    }

    /** 新規 chip: 「開始」「非表示」付きで鳴らす。 */
    fun showChip(ctx: Context, chip: ChipNotice) = post(ctx, chip, STAGE_CHIP, status = null, actions = true)

    /** action 送信中: ボタンを消す。 */
    fun showSending(ctx: Context, chip: ChipNotice) = post(ctx, chip, STAGE_SENDING, status = "送信中…", actions = false)

    /**
     * Worker が受け付けた (202)。agent の結果が chip_result で届くまでボタンは出さない。
     * PC 側は 1 秒未満で押し終わるので、chip_result (FCM) が 202 より先に届くことがある。
     * そのとき「処理中」で上書きすると結果が消えて止まって見えるため、まだ「送信中」のときだけ出す。
     */
    fun showAcceptedIfStillSending(ctx: Context, chip: ChipNotice) {
        if (currentStage(ctx, chip.taskId) != STAGE_SENDING) return
        post(ctx, chip, STAGE_ACCEPTED, status = "Windows で処理中…", actions = false)
    }

    /** 成功: 数秒で自動的に消える。 */
    fun showDone(ctx: Context, chip: ChipNotice, action: String) =
        post(ctx, chip, STAGE_DONE, status = actionDoneLabel(action), actions = false, timeoutMs = DONE_TIMEOUT_MS)

    /** 失敗: メッセージを出してボタンを戻す (もう一度押せる)。 */
    fun showError(ctx: Context, chip: ChipNotice, message: String) =
        post(ctx, chip, STAGE_ERROR, status = message, actions = true)

    fun cancel(ctx: Context, taskId: String) = manager(ctx).cancel(notificationId(taskId))

    /** 表示中の通知の段階。通知が無ければ null。 */
    private fun currentStage(ctx: Context, taskId: String): String? {
        val id = notificationId(taskId)
        return manager(ctx).activeNotifications.firstOrNull { it.id == id }?.notification?.extras
            ?.getString(EXTRA_STAGE)
    }

    /** 表示中の通知から chip を復元する。通知が既に消されていれば null。 */
    fun findChip(ctx: Context, taskId: String): ChipNotice? {
        val id = notificationId(taskId)
        val extras = manager(ctx).activeNotifications.firstOrNull { it.id == id }?.notification?.extras
            ?: return null
        return fromBundle(extras)?.takeIf { it.taskId == taskId }
    }

    fun toBundle(chip: ChipNotice) = Bundle().apply {
        putString(EXTRA_TASK_ID, chip.taskId)
        putString(EXTRA_TITLE, chip.title)
        putString(EXTRA_TLDR, chip.tldr)
        putString(EXTRA_CWD, chip.cwd)
        putString(EXTRA_HOST, chip.host)
        putBoolean(EXTRA_LOCATED, chip.located)
    }

    fun fromBundle(b: Bundle): ChipNotice? {
        val taskId = b.getString(EXTRA_TASK_ID) ?: return null
        return ChipNotice(
            taskId = taskId,
            title = b.getString(EXTRA_TITLE).orEmpty(),
            tldr = b.getString(EXTRA_TLDR).orEmpty(),
            cwd = b.getString(EXTRA_CWD).orEmpty(),
            host = b.getString(EXTRA_HOST).orEmpty(),
            located = b.getBoolean(EXTRA_LOCATED, false),
        )
    }

    private fun post(
        ctx: Context,
        chip: ChipNotice,
        stage: String,
        status: String?,
        actions: Boolean,
        timeoutMs: Long = 0,
    ) {
        val id = notificationId(chip.taskId)
        val body = buildString {
            if (status != null) append(status).append('\n')
            append(chip.tldr)
            if (!chip.located && actions) append('\n').append(NOT_LOCATED_SUFFIX)
        }.trim()
        val builder = NotificationCompat.Builder(ctx, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_chip)
            .setContentTitle(chip.title.ifEmpty { chip.taskId })
            .setContentText(status ?: body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setSubText(listOf(chip.host, chip.cwd).filter { it.isNotEmpty() }.joinToString(" · "))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            // 状態更新 (送信中→結果) のたびに鳴らさない
            .setOnlyAlertOnce(true)
            .setContentIntent(openAppIntent(ctx))
            .addExtras(toBundle(chip).apply { putString(EXTRA_STAGE, stage) })
        if (actions) {
            builder.addAction(0, "開始", actionIntent(ctx, chip, ActionReceiver.ACTION_START))
            builder.addAction(0, "非表示", actionIntent(ctx, chip, ActionReceiver.ACTION_DISMISS))
        }
        if (timeoutMs > 0) builder.setTimeoutAfter(timeoutMs).setAutoCancel(true)
        // POST_NOTIFICATIONS 未許可なら OS が黙って捨てる
        manager(ctx).notify(id, builder.build())
    }

    private fun actionIntent(ctx: Context, chip: ChipNotice, action: String): PendingIntent {
        val intent = Intent(ctx, ActionReceiver::class.java)
            .setAction(ActionReceiver.INTENT_ACTION)
            // data を task×action ごとに変えて PendingIntent を別物にする
            .setData(Uri.parse("chipremote://action/${Uri.encode(chip.taskId)}/$action"))
            .putExtra(ActionReceiver.EXTRA_ACTION, action)
            .putExtras(toBundle(chip))
        return PendingIntent.getBroadcast(
            ctx, notificationId(chip.taskId), intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    private fun openAppIntent(ctx: Context): PendingIntent {
        val intent = Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        return PendingIntent.getActivity(ctx, 0, intent, PendingIntent.FLAG_IMMUTABLE)
    }

    private fun manager(ctx: Context) = ctx.getSystemService(NotificationManager::class.java)
}
