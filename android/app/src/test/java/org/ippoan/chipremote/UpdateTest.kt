package org.ippoan.chipremote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.MessageDigest

class UpdateTest {

    private val sha = "0123456789abcdef".repeat(4)
    private val apk = "https://ippoan.github.io/chip-remote/chip-remote.apk"

    private fun json(
        code: Any? = 12,
        name: Any? = "0.1.12",
        apkUrl: Any? = apk,
        hash: Any? = sha,
    ): String {
        val fields = listOf("versionCode" to code, "versionName" to name, "apk" to apkUrl, "sha256" to hash)
            .filter { it.second != Unit }
            .joinToString(",") { (k, v) ->
                val value = when (v) {
                    null -> "null"
                    is String -> "\"$v\""
                    else -> v.toString()
                }
                "\"$k\":$value"
            }
        return "{$fields}"
    }

    @Test
    fun parsesVersionJson() {
        assertEquals(UpdateInfo(12, "0.1.12", apk, sha), parseUpdateInfo(json()))
    }

    @Test
    fun normalizesShaCaseAndWhitespace() {
        assertEquals(sha, parseUpdateInfo(json(hash = " ${sha.uppercase()}\\n"))?.sha256)
    }

    @Test
    fun versionNameIsOptional() {
        val info = parseUpdateInfo(json(name = Unit))!!
        assertEquals("", info.versionName)
        assertEquals("v12", info.label)
        assertEquals("v0.1.12", parseUpdateInfo(json())!!.label)
    }

    @Test
    fun rejectsBrokenOrIncompleteJson() {
        assertNull(parseUpdateInfo("not json"))
        assertNull(parseUpdateInfo(""))
        assertNull(parseUpdateInfo(json(code = Unit)))
        assertNull(parseUpdateInfo(json(code = 0)))
        assertNull(parseUpdateInfo(json(code = -3)))
        assertNull(parseUpdateInfo(json(apkUrl = Unit)))
        assertNull(parseUpdateInfo(json(apkUrl = null)))
        assertNull(parseUpdateInfo(json(apkUrl = "http://ippoan.github.io/chip-remote/chip-remote.apk")))
        assertNull(parseUpdateInfo(json(hash = Unit)))
        assertNull(parseUpdateInfo(json(hash = null)))
        assertNull(parseUpdateInfo(json(hash = "abc")))
        assertNull(parseUpdateInfo(json(hash = "z".repeat(64))))
    }

    @Test
    fun acceptsVersionCodeAsString() {
        // jq で文字列になっても読める (JSONObject.optInt は数字の文字列を変換する)
        assertEquals(12, parseUpdateInfo("{\"versionCode\":\"12\",\"apk\":\"$apk\",\"sha256\":\"$sha\"}")?.versionCode)
    }

    @Test
    fun newerOnlyWhenVersionCodeIsGreater() {
        val info = UpdateInfo(12, "0.1.12", apk, sha)
        assertTrue(isNewer(info, 11))
        assertFalse(isNewer(info, 12))
        assertFalse(isNewer(info, 13))
    }

    @Test
    fun notifiesNewVersionImmediately() {
        assertTrue(shouldNotify(13, lastNotifiedCode = 12, lastNotifiedAtMs = 1_000, nowMs = 1_001))
        assertTrue(shouldNotify(12, lastNotifiedCode = 0, lastNotifiedAtMs = 0, nowMs = 5))
    }

    @Test
    fun sameVersionAtMostOncePerDay() {
        val day = NOTIFY_INTERVAL_MS
        val t0 = 1_700_000_000_000L
        assertFalse(shouldNotify(12, 12, t0, t0))
        assertFalse(shouldNotify(12, 12, t0, t0 + day - 1))
        assertTrue(shouldNotify(12, 12, t0, t0 + day))
        assertTrue(shouldNotify(12, 12, t0, t0 + 3 * day))
    }

    @Test
    fun clockGoingBackwardsDoesNotSilenceForever() {
        val t0 = 1_700_000_000_000L
        assertTrue(shouldNotify(12, 12, t0, t0 - 1))
    }

    @Test
    fun downloadUrlIsVersionedToBypassCdnCache() {
        assertEquals("$apk?v=12", apkDownloadUrl(UpdateInfo(12, "", apk, sha)))
        assertEquals("$apk?x=1&v=12", apkDownloadUrl(UpdateInfo(12, "", "$apk?x=1", sha)))
        assertEquals("${UpdateSource.VERSION_URL}?t=2", versionCheckUrl(2 * 60_000 + 59_999))
    }

    @Test
    fun hexMatchesSha256sumOutput() {
        // `printf abc | sha256sum`
        val digest = MessageDigest.getInstance("SHA-256").digest("abc".toByteArray())
        assertEquals("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", toHex(digest))
    }

    @Test
    fun installFailureLabels() {
        assertTrue(installFailureLabel(5, "").contains("署名"))
        assertTrue(installFailureLabel(1, "INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match").contains("署名"))
        assertEquals("インストールを中止しました", installFailureLabel(3, ""))
        assertEquals("インストールに失敗しました (status=1)", installFailureLabel(1, "boom"))
    }
}
