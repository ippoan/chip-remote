package org.ippoan.chipremote

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder

/** Worker URL・token・Cloudflare Access の保存先。アプリ専用領域 (allowBackup=false) に置く。 */
object Settings {
    const val DEFAULT_URL = "https://chip-remote.ippoan.org"
    private const val PREFS = "settings"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun url(ctx: Context): String = prefs(ctx).getString("worker_url", null) ?: DEFAULT_URL
    fun token(ctx: Context): String = prefs(ctx).getString("token", null).orEmpty()

    /** id と secret が両方あるときだけ。無ければ Access ヘッダを付けない (Access 有効化前でも動くように)。 */
    fun access(ctx: Context): AccessCredentials? {
        val p = prefs(ctx)
        val id = p.getString("access_client_id", null).orEmpty()
        val secret = p.getString("access_client_secret", null).orEmpty()
        return if (id.isBlank() || secret.isBlank()) null else AccessCredentials(id, secret)
    }

    fun save(ctx: Context, url: String, token: String) {
        prefs(ctx).edit().putString("worker_url", url.trim().trimEnd('/')).putString("token", token.trim()).apply()
    }

    /** null で消す。 */
    fun saveAccess(ctx: Context, access: AccessCredentials?) {
        prefs(ctx).edit()
            .putString("access_client_id", access?.clientId)
            .putString("access_client_secret", access?.clientSecret)
            .apply()
    }

    /** 接続コードの内容をまとめて保存する。コードに access が無ければ Access 設定は消す。 */
    fun save(ctx: Context, code: ConnectCode) {
        save(ctx, code.url, code.token)
        saveAccess(ctx, code.access)
    }

    /** URL と token が揃っていれば ApiClient、無ければ null。Access があればヘッダに載せる。 */
    fun client(ctx: Context): ApiClient? {
        val url = url(ctx)
        val token = token(ctx)
        return if (url.isBlank() || token.isBlank()) null else ApiClient(url, token, access(ctx))
    }
}

sealed class ActionOutcome {
    /** 202。結果は FCM chip_result で届く */
    object Accepted : ActionOutcome()
    object AgentOffline : ActionOutcome()
    object ChipClosed : ActionOutcome()
    data class Failed(val message: String) : ActionOutcome()
}

class ApiException(val code: Int, val errorCode: String?, message: String) : IOException(message)

/** docs/PROTOCOL.md の phone 側 HTTP。すべて Bearer 認証 + (設定済みなら) Cloudflare Access の service token。 */
class ApiClient(
    private val baseUrl: String,
    private val token: String,
    private val access: AccessCredentials? = null,
) {

    suspend fun registerDevice(fcmToken: String, name: String) {
        val body = JSONObject().put("fcm_token", fcmToken).put("name", name)
        val res = request("POST", "/v1/devices", body)
        if (res.code !in 200..299) throw httpError(res)
    }

    suspend fun openChips(): List<Chip> {
        val res = request("GET", "/v1/chips?open=1", null)
        if (res.code !in 200..299) throw httpError(res)
        return parseChips(res.text)
    }

    /** ネットワーク例外も Failed に畳む (呼び出し側は通知を更新するだけなので)。 */
    suspend fun postAction(taskId: String, action: String): ActionOutcome = try {
        val path = "/v1/chips/" + URLEncoder.encode(taskId, "UTF-8") + "/action"
        val res = request("POST", path, JSONObject().put("action", action))
        when {
            res.code in 200..299 -> ActionOutcome.Accepted
            res.code == 409 && parseErrorCode(res.text) == "agent_offline" -> ActionOutcome.AgentOffline
            res.code == 409 && parseErrorCode(res.text) == "chip_closed" -> ActionOutcome.ChipClosed
            else -> ActionOutcome.Failed(httpError(res).message.orEmpty())
        }
    } catch (e: IOException) {
        ActionOutcome.Failed("通信エラー: ${e.message ?: e.javaClass.simpleName}")
    }

    private class Response(val code: Int, val text: String, val location: String?, val contentType: String?)

    private fun httpError(res: Response): ApiException {
        if (isAccessDenied(res.code, res.location, res.contentType, res.text)) {
            return ApiException(res.code, "access_denied", ACCESS_DENIED_MESSAGE)
        }
        val err = parseErrorCode(res.text)
        val msg = when {
            res.code == 401 -> "認証エラー (token を確認してください)"
            err != null -> "HTTP ${res.code}: $err"
            else -> "HTTP ${res.code}"
        }
        return ApiException(res.code, err, msg)
    }

    private suspend fun request(method: String, path: String, body: JSONObject?): Response =
        withContext(Dispatchers.IO) {
            val conn = URL(baseUrl.trimEnd('/') + path).openConnection() as HttpURLConnection
            try {
                conn.requestMethod = method
                // 通知ボタンからは BroadcastReceiver.goAsync (約 10 秒) の中で呼ぶので短めに
                conn.connectTimeout = 4_000
                conn.readTimeout = 5_000
                // Access のログイン画面へのリダイレクトを追うと 200 の HTML になって見分けられないので追わない
                conn.instanceFollowRedirects = false
                for ((k, v) in workerHeaders(token, access)) conn.setRequestProperty(k, v)
                if (body != null) {
                    conn.doOutput = true
                    conn.setRequestProperty("Content-Type", "application/json; charset=utf-8")
                    conn.outputStream.use { it.write(body.toString().toByteArray(Charsets.UTF_8)) }
                }
                val code = conn.responseCode
                val stream = if (code >= 400) conn.errorStream else conn.inputStream
                val text = stream?.bufferedReader(Charsets.UTF_8)?.use { it.readText() }.orEmpty()
                Response(code, text, conn.getHeaderField("Location"), conn.contentType)
            } finally {
                conn.disconnect()
            }
        }
}
