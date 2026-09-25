package org.ippoan.chipremote

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit

/**
 * version.json を見て、新しい版があれば「更新があります」通知を出す。
 * 通知タップ → MainActivity (EXTRA_START_UPDATE) → Updater でダウンロード + インストール。
 */
object UpdateChecker {
    private const val TAG = "UpdateChecker"
    const val CHANNEL_ID = "updates"
    const val NOTIFICATION_ID = 0x0c1f_0001
    private const val PREFS = "updates"
    private const val KEY_LAST_CODE = "last_notified_code"
    private const val KEY_LAST_AT = "last_notified_at"
    private const val WORK_NAME = "update-check"

    fun ensureChannel(ctx: Context) {
        val channel = NotificationChannel(CHANNEL_ID, "アプリの更新", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "chip-remote の新しい版のお知らせ"
        }
        ctx.getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    /** 6 時間ごとの確認を登録する (登録済みならそのまま)。アプリを開かなくても通知が出るように。 */
    fun schedule(ctx: Context) {
        val request = PeriodicWorkRequestBuilder<UpdateWorker>(6, TimeUnit.HOURS)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .build()
        WorkManager.getInstance(ctx)
            .enqueueUniquePeriodicWork(WORK_NAME, ExistingPeriodicWorkPolicy.KEEP, request)
    }

    /** version.json を取る。取れない / 壊れていれば例外。 */
    suspend fun fetch(): UpdateInfo = withContext(Dispatchers.IO) {
        val conn = URL(versionCheckUrl(System.currentTimeMillis())).openConnection() as HttpURLConnection
        try {
            conn.connectTimeout = 15_000
            conn.readTimeout = 15_000
            conn.useCaches = false
            conn.setRequestProperty("Cache-Control", "no-cache")
            val code = conn.responseCode
            if (code != HttpURLConnection.HTTP_OK) throw IOException("version.json: HTTP $code")
            val body = conn.inputStream.bufferedReader().use { it.readText() }
            parseUpdateInfo(body) ?: throw IOException("version.json の形式が不正です")
        } finally {
            conn.disconnect()
        }
    }

    /**
     * 自動確認 (起動時 / WorkManager)。新しい版があれば 1 日 1 回まで通知する。
     * 新しい版が無ければ (更新済みなら) 残っている更新通知を消す。戻り値は新しい版 (無ければ null)。
     */
    suspend fun checkAndNotify(ctx: Context): UpdateInfo? {
        val info = fetch()
        if (!isNewer(info, BuildConfig.VERSION_CODE)) {
            cancelNotification(ctx)
            return null
        }
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val now = System.currentTimeMillis()
        if (shouldNotify(info.versionCode, prefs.getInt(KEY_LAST_CODE, 0), prefs.getLong(KEY_LAST_AT, 0), now)) {
            notifyAvailable(ctx, info)
            prefs.edit().putInt(KEY_LAST_CODE, info.versionCode).putLong(KEY_LAST_AT, now).apply()
        }
        return info
    }

    private fun notifyAvailable(ctx: Context, info: UpdateInfo) {
        val intent = Intent(ctx, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            .putExtra(MainActivity.EXTRA_START_UPDATE, true)
        val pi = PendingIntent.getActivity(
            ctx, NOTIFICATION_ID, intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = NotificationCompat.Builder(ctx, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_chip)
            .setContentTitle("chip-remote の更新があります (${info.label})")
            .setContentText("タップして更新")
            .setContentIntent(pi)
            .setAutoCancel(true)
            .build()
        // POST_NOTIFICATIONS 未許可なら OS が黙って捨てる
        ctx.getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification)
    }

    fun cancelNotification(ctx: Context) =
        ctx.getSystemService(NotificationManager::class.java).cancel(NOTIFICATION_ID)

    /** 失敗・インストール待ちなど、更新の進み具合をアプリ外でも見えるように出す。 */
    fun notifyStatus(ctx: Context, text: String, contentIntent: PendingIntent? = null) {
        val notification = NotificationCompat.Builder(ctx, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_chip)
            .setContentTitle("chip-remote の更新")
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setAutoCancel(true)
            .apply { if (contentIntent != null) setContentIntent(contentIntent) }
            .build()
        ctx.getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification)
    }

    internal fun logFailure(e: Exception) = Log.w(TAG, "update check failed: ${e.message}")
}

/** 6 時間ごとの更新確認。失敗しても retry せず次の周期に任せる (通知は急がない)。 */
class UpdateWorker(ctx: Context, params: WorkerParameters) : CoroutineWorker(ctx, params) {
    override suspend fun doWork(): Result {
        try {
            UpdateChecker.checkAndNotify(applicationContext)
        } catch (e: Exception) {
            UpdateChecker.logFailure(e)
        }
        return Result.success()
    }
}
