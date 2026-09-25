package org.ippoan.chipremote

import org.json.JSONObject

/**
 * 自己更新まわりの純粋関数 (JVM テスト対象)。Android API に触る部分は UpdateChecker / Updater。
 *
 * 配布は GitHub Pages のみ (Release asset は署名付き URL へのリダイレクトで Android の DL が止まるため)。
 * version.json は CI (.github/workflows/android.yml) が APK と同じ gh-pages コミットで置く。
 */
object UpdateSource {
    const val VERSION_URL = "https://ippoan.github.io/chip-remote/version.json"
}

/** version.json の中身。 */
data class UpdateInfo(
    val versionCode: Int,
    val versionName: String,
    val apk: String,
    /** 小文字 16 進 64 文字 */
    val sha256: String,
) {
    /** 画面・通知に出す版の表記。versionName が無ければ versionCode。 */
    val label: String get() = "v" + versionName.ifBlank { versionCode.toString() }
}

private val SHA256_HEX = Regex("^[0-9a-f]{64}$")

/**
 * version.json を読む。壊れている / 必須項目が欠けている / https でない場合は null (更新しない)。
 * `{"versionCode": 12, "versionName": "0.1.12", "apk": "https://…/chip-remote.apk", "sha256": "…"}`
 */
fun parseUpdateInfo(json: String): UpdateInfo? = try {
    val o = JSONObject(json)
    val code = o.optInt("versionCode", 0)
    val apk = if (o.isNull("apk")) "" else o.optString("apk", "").trim()
    val sha = if (o.isNull("sha256")) "" else o.optString("sha256", "").trim().lowercase()
    val name = if (o.isNull("versionName")) "" else o.optString("versionName", "").trim()
    if (code <= 0 || !apk.startsWith("https://") || !SHA256_HEX.matches(sha)) null
    else UpdateInfo(code, name, apk, sha)
} catch (e: Exception) {
    null
}

/** 配布中の版が今の版より新しいか。 */
fun isNewer(info: UpdateInfo, currentVersionCode: Int): Boolean = info.versionCode > currentVersionCode

const val NOTIFY_INTERVAL_MS: Long = 24L * 60 * 60 * 1000

/**
 * 更新通知を出してよいか。同じ versionCode は 1 日 1 回まで。
 * 前回と違う版なら即出す。時計が巻き戻っていたら (now < lastAt) 出す (永久に黙らないように)。
 */
fun shouldNotify(
    versionCode: Int,
    lastNotifiedCode: Int,
    lastNotifiedAtMs: Long,
    nowMs: Long,
    intervalMs: Long = NOTIFY_INTERVAL_MS,
): Boolean {
    if (versionCode != lastNotifiedCode) return true
    if (nowMs < lastNotifiedAtMs) return true
    return nowMs - lastNotifiedAtMs >= intervalMs
}

/**
 * APK の取得 URL。版ごとに URL を変えて Pages の CDN キャッシュ (max-age=600) に古い APK を掴まされないようにする
 * (古い APK を掴むと sha256 が合わない)。
 */
fun apkDownloadUrl(info: UpdateInfo): String {
    val sep = if (info.apk.contains('?')) '&' else '?'
    return "${info.apk}${sep}v=${info.versionCode}"
}

/** version.json も CDN キャッシュを避ける (分単位で URL を変える)。 */
fun versionCheckUrl(nowMs: Long): String = "${UpdateSource.VERSION_URL}?t=${nowMs / 60_000}"

fun toHex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

/** PackageInstaller の失敗 status を画面表示用の日本語にする (値は PackageInstaller.STATUS_*)。 */
fun installFailureLabel(status: Int, message: String): String = when {
    // STATUS_FAILURE_CONFLICT(5) の最頻原因は署名違い (INSTALL_FAILED_UPDATE_INCOMPATIBLE。CI の debug 鍵、ローカル build など)
    status == 5 || message.contains("signature", ignoreCase = true) ->
        "署名が異なるため上書きできません。アンインストールしてから入れ直してください"
    status == 3 -> "インストールを中止しました" // STATUS_FAILURE_ABORTED (確認画面でキャンセル)
    status == 2 -> "インストールがブロックされました" // STATUS_FAILURE_BLOCKED
    status == 6 -> "空き容量が足りません" // STATUS_FAILURE_STORAGE
    status == 4 -> "APK が不正です" // STATUS_FAILURE_INVALID
    status == 7 -> "この端末にはインストールできません" // STATUS_FAILURE_INCOMPATIBLE
    else -> "インストールに失敗しました (status=$status)"
}
