package org.ippoan.chipremote

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder

/** Worker URL と token の保存先。token はアプリ専用領域 (allowBackup=false) に置く。 */
object Settings {
    const val DEFAULT_URL = "https://chip-remote.ippoan.org"
    private const val PREFS = "settings"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun url(ctx: Context): String = prefs(ctx).getString("worker_url", null) ?: DEFAULT_URL
    fun token(ctx: Context): String = prefs(ctx).getString("token", null).orEmpty()

    fun save(ctx: Context, url: String, token: String) {
        prefs(ctx).edit().putString("worker_url", url.trim().trimEnd('/')).putString("token", token.trim()).apply()
    }

    /** URL と token が揃っていれば ApiClient、無ければ null。 */
    fun client(ctx: Context): ApiClient? {
        val url = url(ctx)
        val token = token(ctx)
        return if (url.isBlank() || token.isBlank()) null else ApiClient(url, token)
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

/** docs/PROTOCOL.md の phone 側 HTTP。すべて Bearer 認証。 */
class ApiClient(private val baseUrl: String, private val token: String) {

    suspend fun registerDevice(fcmToken: String, name: String) {
        val body = JSONObject().put("fcm_token", fcmToken).put("name", name)
        val (code, text) = request("POST", "/v1/devices", body)
        if (code !in 200..299) throw httpError(code, text)
    }

    suspend fun openChips(): List<Chip> {
        val (code, text) = request("GET", "/v1/chips?open=1", null)
        if (code !in 200..299) throw httpError(code, text)
        return parseChips(text)
    }

    /** ネットワーク例外も Failed に畳む (呼び出し側は通知を更新するだけなので)。 */
    suspend fun postAction(taskId: String, action: String): ActionOutcome = try {
        val path = "/v1/chips/" + URLEncoder.encode(taskId, "UTF-8") + "/action"
        val (code, text) = request("POST", path, JSONObject().put("action", action))
        when {
            code in 200..299 -> ActionOutcome.Accepted
            code == 409 && parseErrorCode(text) == "agent_offline" -> ActionOutcome.AgentOffline
            code == 409 && parseErrorCode(text) == "chip_closed" -> ActionOutcome.ChipClosed
            else -> ActionOutcome.Failed(httpError(code, text).message.orEmpty())
        }
    } catch (e: IOException) {
        ActionOutcome.Failed("通信エラー: ${e.message ?: e.javaClass.simpleName}")
    }

    private fun httpError(code: Int, text: String): ApiException {
        val err = parseErrorCode(text)
        val msg = when {
            code == 401 -> "認証エラー (token を確認してください)"
            err != null -> "HTTP $code: $err"
            else -> "HTTP $code"
        }
        return ApiException(code, err, msg)
    }

    private suspend fun request(method: String, path: String, body: JSONObject?): Pair<Int, String> =
        withContext(Dispatchers.IO) {
            val conn = URL(baseUrl.trimEnd('/') + path).openConnection() as HttpURLConnection
            try {
                conn.requestMethod = method
                // 通知ボタンからは BroadcastReceiver.goAsync (約 10 秒) の中で呼ぶので短めに
                conn.connectTimeout = 4_000
                conn.readTimeout = 5_000
                conn.setRequestProperty("Authorization", "Bearer $token")
                conn.setRequestProperty("Accept", "application/json")
                if (body != null) {
                    conn.doOutput = true
                    conn.setRequestProperty("Content-Type", "application/json; charset=utf-8")
                    conn.outputStream.use { it.write(body.toString().toByteArray(Charsets.UTF_8)) }
                }
                val code = conn.responseCode
                val stream = if (code >= 400) conn.errorStream else conn.inputStream
                val text = stream?.bufferedReader(Charsets.UTF_8)?.use { it.readText() }.orEmpty()
                code to text
            } finally {
                conn.disconnect()
            }
        }
}
