package org.ippoan.chipremote

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.util.Log
import androidx.core.content.IntentCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/** 更新の進み具合 (MainActivity が表示する)。 */
sealed class UpdateState {
    object Idle : UpdateState()
    data class Downloading(val info: UpdateInfo, val percent: Int) : UpdateState()
    /** PackageInstaller に渡した。OS の確認画面でユーザーが「更新」を押すのを待っている。 */
    data class Installing(val info: UpdateInfo) : UpdateState()
    data class Failed(val message: String) : UpdateState()
}

/**
 * APK を Pages から取ってきて PackageInstaller で入れる。
 * 普通の個人端末 (device owner ではない) なので、OS のインストール確認画面は必ず出る (ユーザーがタップする)。
 * 呼ぶ前に packageManager.canRequestPackageInstalls() を確認すること (MainActivity が設定画面へ誘導する)。
 */
object Updater {
    private const val TAG = "Updater"
    private const val APK_NAME = "update.apk"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val _state = MutableStateFlow<UpdateState>(UpdateState.Idle)
    val state: StateFlow<UpdateState> = _state.asStateFlow()

    /** MainActivity が前面にいるか。前面なら確認画面を直接開ける (バックグラウンドからの Activity 起動は OS に止められる)。 */
    @Volatile
    var foreground: Boolean = false

    val busy: Boolean get() = _state.value is UpdateState.Downloading

    fun start(ctx: Context, info: UpdateInfo) {
        if (busy) return
        val app = ctx.applicationContext
        _state.value = UpdateState.Downloading(info, 0)
        scope.launch {
            try {
                val apk = download(app, info)
                install(app, info, apk)
                _state.value = UpdateState.Installing(info)
            } catch (e: Exception) {
                Log.e(TAG, "update failed", e)
                fail(app, "更新に失敗: ${e.message ?: e.javaClass.simpleName}")
            }
        }
    }

    internal fun fail(ctx: Context, message: String) {
        _state.value = UpdateState.Failed(message)
        if (!foreground) UpdateChecker.notifyStatus(ctx, message)
    }

    /** cache に落として sha256 を確かめる。合わなければ消して例外。 */
    private fun download(ctx: Context, info: UpdateInfo): File {
        val file = File(ctx.cacheDir, APK_NAME)
        val digest = MessageDigest.getInstance("SHA-256")
        val conn = URL(apkDownloadUrl(info)).openConnection() as HttpURLConnection
        try {
            conn.instanceFollowRedirects = true
            conn.connectTimeout = 30_000
            conn.readTimeout = 60_000
            conn.useCaches = false
            val code = conn.responseCode
            if (code != HttpURLConnection.HTTP_OK) throw IOException("APK: HTTP $code")
            val total = conn.contentLengthLong
            var done = 0L
            var lastPercent = -1
            conn.inputStream.use { input ->
                file.outputStream().use { output ->
                    val buf = ByteArray(64 * 1024)
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        output.write(buf, 0, n)
                        digest.update(buf, 0, n)
                        done += n
                        if (total > 0) {
                            val p = (done * 100 / total).toInt()
                            if (p != lastPercent) {
                                lastPercent = p
                                _state.value = UpdateState.Downloading(info, p)
                            }
                        }
                    }
                }
            }
        } catch (e: Exception) {
            file.delete()
            throw e
        } finally {
            conn.disconnect()
        }
        val actual = toHex(digest.digest())
        if (actual != info.sha256) {
            file.delete()
            Log.w(TAG, "sha256 mismatch: expected=${info.sha256} actual=$actual")
            throw IOException("ダウンロードした APK が壊れています (sha256 不一致)。少し待ってからやり直してください")
        }
        return file
    }

    private fun install(ctx: Context, info: UpdateInfo, apk: File) {
        val installer = ctx.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
            .apply { setAppPackageName(ctx.packageName) }
        val sessionId = installer.createSession(params)
        val session = installer.openSession(sessionId)
        try {
            apk.inputStream().use { input ->
                session.openWrite("chip-remote.apk", 0, apk.length()).use { output ->
                    input.copyTo(output)
                    session.fsync(output)
                }
            }
            val intent = Intent(ctx, InstallResultReceiver::class.java)
                .setAction(InstallResultReceiver.ACTION)
                .putExtra(InstallResultReceiver.EXTRA_VERSION_LABEL, info.label)
            // 結果の extras は OS が書き足すので MUTABLE (明示 Intent なので API 34 の制限にもかからない)
            val pi = PendingIntent.getBroadcast(
                ctx, sessionId, intent,
                PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            session.commit(pi.intentSender)
        } catch (e: Exception) {
            session.abandon()
            throw e
        } finally {
            session.close()
            apk.delete()
        }
    }

    internal fun installFinished(message: String?) {
        _state.value = if (message == null) UpdateState.Idle else UpdateState.Failed(message)
    }
}

/** PackageInstaller セッションの結果。STATUS_PENDING_USER_ACTION なら OS の確認画面を出す。 */
class InstallResultReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION) return
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
        val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE).orEmpty()
        when (status) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java)
                    ?: return Updater.fail(context, "インストール画面を開けませんでした")
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                if (Updater.foreground) {
                    context.startActivity(confirm)
                } else {
                    // バックグラウンドからは確認画面を開けないので、通知のタップで開いてもらう
                    val label = intent.getStringExtra(EXTRA_VERSION_LABEL).orEmpty()
                    val pi = PendingIntent.getActivity(
                        context, UpdateChecker.NOTIFICATION_ID, confirm,
                        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                    )
                    UpdateChecker.notifyStatus(context, "タップして $label をインストール", pi)
                }
            }
            // 自分自身の更新なので、成功時はふつうこのプロセスごと入れ替わってここには来ない
            PackageInstaller.STATUS_SUCCESS -> Updater.installFinished(null)
            else -> {
                Log.w("InstallResult", "install failed: status=$status message=$message")
                val label = installFailureLabel(status, message)
                Updater.installFinished(label)
                if (!Updater.foreground && status != PackageInstaller.STATUS_FAILURE_ABORTED) {
                    UpdateChecker.notifyStatus(context, label)
                }
            }
        }
    }

    companion object {
        const val ACTION = "org.ippoan.chipremote.INSTALL_RESULT"
        const val EXTRA_VERSION_LABEL = "version_label"
    }
}
