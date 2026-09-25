package org.ippoan.chipremote

import org.json.JSONObject

/** 通知 1 件分の chip 情報 (FCM `chip` と、通知 extras / PendingIntent extras の往復に使う)。 */
data class ChipNotice(
    val taskId: String,
    val title: String,
    val tldr: String,
    val cwd: String,
    val host: String,
    val located: Boolean,
)

/** GET /v1/chips?open=1 の 1 行。 */
data class Chip(
    val taskId: String,
    val title: String,
    val tldr: String,
    val cwd: String,
    val host: String,
    val status: String,
    val located: Boolean,
    val error: String?,
) {
    fun toNotice() = ChipNotice(taskId, title, tldr, cwd, host, located)
}

/** FCM data payload (docs/PROTOCOL.md「FCM」)。 */
sealed class FcmEvent {
    data class New(val chip: ChipNotice) : FcmEvent()
    data class Result(val taskId: String, val action: String, val ok: Boolean, val error: String) : FcmEvent()
    data class Cancel(val taskId: String) : FcmEvent()

    companion object {
        /** 未知の type や task_id 欠落は null (無視する)。 */
        fun parse(data: Map<String, String>): FcmEvent? {
            val taskId = data["task_id"]?.takeIf { it.isNotBlank() } ?: return null
            return when (data["type"]) {
                "chip" -> New(
                    ChipNotice(
                        taskId = taskId,
                        title = data["title"].orEmpty(),
                        tldr = data["tldr"].orEmpty(),
                        cwd = data["cwd"].orEmpty(),
                        host = data["host"].orEmpty(),
                        // 欠落時は「確認できた」扱いにしない
                        located = data["located"] == "true",
                    )
                )
                "chip_result" -> Result(
                    taskId = taskId,
                    action = data["action"].orEmpty(),
                    ok = data["ok"] == "true",
                    error = data["error"].orEmpty(),
                )
                "chip_cancel" -> Cancel(taskId)
                else -> null
            }
        }
    }
}

/** 通知 ID は task_id.hashCode() (PROTOCOL.md)。String.hashCode は仕様で固定なので再起動を跨いでも同じ。 */
fun notificationId(taskId: String): Int = taskId.hashCode()

fun parseChips(json: String): List<Chip> {
    val arr = JSONObject(json).optJSONArray("chips") ?: return emptyList()
    return (0 until arr.length()).map { i ->
        val o = arr.getJSONObject(i)
        Chip(
            taskId = o.getString("task_id"),
            title = o.optStringOrEmpty("title"),
            tldr = o.optStringOrEmpty("tldr"),
            cwd = o.optStringOrEmpty("cwd"),
            host = o.optStringOrEmpty("host"),
            status = o.optStringOrEmpty("status"),
            located = o.optBoolean("located", false),
            error = o.optStringOrEmpty("error").ifEmpty { null },
        )
    }
}

/** `{"error":"agent_offline"}` の error を取り出す。JSON でなければ null。 */
fun parseErrorCode(body: String): String? =
    try {
        JSONObject(body).optStringOrEmpty("error").ifEmpty { null }
    } catch (e: Exception) {
        null
    }

// optString は null を "null" にするので自前で
private fun JSONObject.optStringOrEmpty(key: String): String =
    if (isNull(key)) "" else optString(key, "")

/** Worker / agent の error コードを画面表示用の日本語にする。 */
fun errorLabel(code: String): String = when (code) {
    "agent_offline" -> "Windows の agent がオフラインです"
    "agent_timeout" -> "Windows の agent が応答しませんでした"
    "not_found" -> "画面で chip が見つかりませんでした"
    "chip_closed" -> "この chip はもう閉じています"
    "" -> "失敗しました"
    else -> "失敗: $code"
}

fun actionDoneLabel(action: String): String = when (action) {
    "start" -> "開始しました"
    "dismiss" -> "非表示にしました"
    else -> "完了しました"
}
