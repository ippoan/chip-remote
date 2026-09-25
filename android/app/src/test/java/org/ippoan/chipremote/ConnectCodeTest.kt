package org.ippoan.chipremote

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Base64

class ConnectCodeTest {

    /** agent 側と同じ作り方: base64url・パディング無し。 */
    private fun encode(json: String): String =
        CONNECT_CODE_PREFIX + Base64.getUrlEncoder().withoutPadding().encodeToString(json.toByteArray(Charsets.UTF_8))

    private fun encode(vararg pairs: Pair<String, Any?>): String {
        val o = JSONObject()
        for ((k, v) in pairs) o.put(k, v ?: JSONObject.NULL)
        return encode(o.toString())
    }

    @Test
    fun parsesFullCode() {
        val text = encode(
            "url" to "https://chip-remote.ippoan.org",
            "token" to "tok",
            "accessClientId" to "id.access",
            "accessClientSecret" to "sec",
        )
        assertEquals(
            ConnectCode("https://chip-remote.ippoan.org", "tok", AccessCredentials("id.access", "sec")),
            parseConnectCode(text),
        )
    }

    @Test
    fun fixedVector() {
        // agent 実装と突き合わせるための固定値 ({"url":"https://x.example","token":"t"})
        assertEquals(
            ConnectCode("https://x.example", "t", null),
            parseConnectCode("chipremote:eyJ1cmwiOiJodHRwczovL3guZXhhbXBsZSIsInRva2VuIjoidCJ9"),
        )
    }

    @Test
    fun accessFieldsAreOptional() {
        assertEquals(
            ConnectCode("https://x.example", "t", null),
            parseConnectCode(encode("url" to "https://x.example/", "token" to "t")),
        )
        // 空文字 / null も「無し」扱い
        assertEquals(
            ConnectCode("https://x.example", "t", null),
            parseConnectCode(encode("url" to "https://x.example", "token" to "t", "accessClientId" to "", "accessClientSecret" to null)),
        )
    }

    @Test
    fun rejectsHalfAccess() {
        assertNull(parseConnectCode(encode("url" to "https://x.example", "token" to "t", "accessClientId" to "id")))
        assertNull(parseConnectCode(encode("url" to "https://x.example", "token" to "t", "accessClientSecret" to "s")))
    }

    @Test
    fun toleratesWhitespaceAndPadding() {
        val code = encode("url" to "https://x.example", "token" to "t", "accessClientId" to "i", "accessClientSecret" to "s")
        val expected = ConnectCode("https://x.example", "t", AccessCredentials("i", "s"))
        assertEquals(expected, parseConnectCode("  \n$code\r\n "))
        // 途中で折り返されたもの
        val mid = code.length / 2
        assertEquals(expected, parseConnectCode(code.substring(0, mid) + "\n" + code.substring(mid)))
        // パディング付き
        val padded = CONNECT_CODE_PREFIX + Base64.getUrlEncoder().encodeToString(
            """{"url":"https://x.example","token":"t","accessClientId":"i","accessClientSecret":"s"}""".toByteArray()
        )
        assertEquals(expected, parseConnectCode(padded))
    }

    @Test
    fun rejectsBadPrefix() {
        val body = encode("url" to "https://x.example", "token" to "t").removePrefix(CONNECT_CODE_PREFIX)
        assertNull(parseConnectCode(body))
        assertNull(parseConnectCode("https://x.example"))
        assertNull(parseConnectCode("chip-remote:$body"))
        assertNull(parseConnectCode(""))
        assertNull(parseConnectCode(CONNECT_CODE_PREFIX))
    }

    @Test
    fun rejectsBadBase64() {
        assertNull(parseConnectCode("chipremote:@@@not base64!!"))
        // 標準 base64 の + / は base64url では不正
        assertNull(parseConnectCode("chipremote:ab+/cd"))
    }

    @Test
    fun rejectsBadJson() {
        assertNull(parseConnectCode(encode("not json")))
        assertNull(parseConnectCode(encode("""["https://x.example","t"]""")))
    }

    @Test
    fun rejectsBlankOrInvalidRequired() {
        assertNull(parseConnectCode(encode("url" to "", "token" to "t")))
        assertNull(parseConnectCode(encode("url" to "https://x.example", "token" to "  ")))
        assertNull(parseConnectCode(encode("token" to "t")))
        assertNull(parseConnectCode(encode("url" to "https://x.example")))
        assertNull(parseConnectCode(encode("url" to "ftp://x.example", "token" to "t")))
        // 文字列以外は受け付けない
        assertNull(parseConnectCode(encode("url" to "https://x.example", "token" to 123)))
    }

    @Test
    fun trimsValues() {
        assertEquals(
            ConnectCode("https://x.example", "t", AccessCredentials("i", "s")),
            parseConnectCode(
                encode("url" to " https://x.example/ ", "token" to " t ", "accessClientId" to " i", "accessClientSecret" to "s ")
            ),
        )
    }

    @Test
    fun headersIncludeAccessOnlyWhenSet() {
        assertEquals(
            mapOf("Authorization" to "Bearer t", "Accept" to "application/json"),
            workerHeaders("t", null),
        )
        assertEquals(
            mapOf(
                "Authorization" to "Bearer t",
                "Accept" to "application/json",
                "CF-Access-Client-Id" to "i",
                "CF-Access-Client-Secret" to "s",
            ),
            workerHeaders("t", AccessCredentials("i", "s")),
        )
    }

    @Test
    fun detectsAccessDenial() {
        assertTrue(
            isAccessDenied(302, "https://ippoan.cloudflareaccess.com/cdn-cgi/access/login/chip-remote.ippoan.org?kid=x", null, "")
        )
        assertTrue(isAccessDenied(302, "https://chip-remote.ippoan.org/cdn-cgi/access/login", null, ""))
        assertTrue(isAccessDenied(403, null, "text/html; charset=UTF-8", "<html>Forbidden</html>"))
        assertTrue(isAccessDenied(401, null, null, "<a href=\"https://x.cloudflareaccess.com\">"))
        // Worker 自身の JSON エラーは Access 扱いしない
        assertFalse(isAccessDenied(401, null, "application/json", """{"error":"unauthorized"}"""))
        assertFalse(isAccessDenied(409, null, "text/html", "<html></html>"))
        assertFalse(isAccessDenied(302, "https://example.com/elsewhere", null, ""))
        assertFalse(isAccessDenied(302, null, null, ""))
        assertFalse(isAccessDenied(200, null, "text/html", "cloudflareaccess.com"))
    }
}
