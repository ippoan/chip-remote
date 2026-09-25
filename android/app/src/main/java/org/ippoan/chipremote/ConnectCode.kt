package org.ippoan.chipremote

import org.json.JSONObject
import java.util.Base64

/** Cloudflare Access の service token。Worker 宛の全リクエスト (/health 以外) に付ける。 */
data class AccessCredentials(val clientId: String, val clientSecret: String)

/**
 * 接続コード 1 つで URL / token / Access をまとめて設定する。
 *
 * 形式: `chipremote:` + base64url (パディング無し) の UTF-8 JSON
 * `{"url": "...", "token": "...", "accessClientId": "...", "accessClientSecret": "..."}`
 * (access の 2 項目は省略可)。PC の agent が QR で表示し、スマホはカメラで読むか文字列を貼り付ける。
 */
data class ConnectCode(val url: String, val token: String, val access: AccessCredentials?)

const val CONNECT_CODE_PREFIX = "chipremote:"

/**
 * 壊れている / 必須項目が空 / access が片方だけ、のときは null。
 * 貼り付け時の前後の空白・改行や、途中で折り返された改行は無視する。パディング (=) があっても受け付ける。
 */
fun parseConnectCode(text: String): ConnectCode? {
    val s = text.trim()
    if (!s.startsWith(CONNECT_CODE_PREFIX, ignoreCase = true)) return null
    val payload = s.substring(CONNECT_CODE_PREFIX.length).filterNot { it.isWhitespace() }
    if (payload.isEmpty()) return null
    val json = try {
        JSONObject(String(Base64.getUrlDecoder().decode(payload), Charsets.UTF_8))
    } catch (e: Exception) {
        // IllegalArgumentException (base64) / JSONException
        return null
    }
    val url = json.stringOrEmpty("url").trimEnd('/')
    val token = json.stringOrEmpty("token")
    if (token.isEmpty() || !(url.startsWith("https://") || url.startsWith("http://"))) return null
    val id = json.stringOrEmpty("accessClientId")
    val secret = json.stringOrEmpty("accessClientSecret")
    val access = when {
        id.isEmpty() && secret.isEmpty() -> null
        id.isEmpty() || secret.isEmpty() -> return null
        else -> AccessCredentials(id, secret)
    }
    return ConnectCode(url, token, access)
}

private fun JSONObject.stringOrEmpty(key: String): String =
    (opt(key) as? String)?.trim().orEmpty()

/** Worker へのリクエストヘッダ。access が無ければ Bearer だけ (Access 有効化前の Worker でも動く)。 */
fun workerHeaders(token: String, access: AccessCredentials?): Map<String, String> = buildMap {
    put("Authorization", "Bearer $token")
    put("Accept", "application/json")
    if (access != null) {
        put("CF-Access-Client-Id", access.clientId)
        put("CF-Access-Client-Secret", access.clientSecret)
    }
}

/**
 * Cloudflare Access に止められた応答か。Worker 自身は JSON しか返さないので、
 * ログイン画面 (*.cloudflareaccess.com) へのリダイレクトか、401/403 の HTML は Access 由来とみなす。
 */
fun isAccessDenied(code: Int, location: String?, contentType: String?, body: String): Boolean {
    if (code in 300..399) {
        val host = location?.let { runCatching { java.net.URI(it).host }.getOrNull() }.orEmpty()
        return host.endsWith(".cloudflareaccess.com", ignoreCase = true) ||
            location.orEmpty().contains("/cdn-cgi/access/", ignoreCase = true)
    }
    if (code == 401 || code == 403) {
        return contentType.orEmpty().contains("text/html", ignoreCase = true) ||
            body.contains("cloudflareaccess.com", ignoreCase = true) ||
            body.contains("Cloudflare Access", ignoreCase = true)
    }
    return false
}

const val ACCESS_DENIED_MESSAGE = "Cloudflare Access に拒否されました (接続コードを読み込み直してください)"
