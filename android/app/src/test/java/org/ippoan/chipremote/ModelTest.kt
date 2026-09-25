package org.ippoan.chipremote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ModelTest {

    @Test
    fun parsesChip() {
        val ev = FcmEvent.parse(
            mapOf(
                "type" to "chip", "task_id" to "task_12d25b98", "title" to "T", "tldr" to "why",
                "cwd" to "/home/claude/x", "host" to "mini-ryzen", "located" to "false",
            )
        )
        assertEquals(
            FcmEvent.New(ChipNotice("task_12d25b98", "T", "why", "/home/claude/x", "mini-ryzen", located = false)),
            ev,
        )
    }

    @Test
    fun locatedTrueOnlyWhenExplicit() {
        val located = { v: String? ->
            val m = mutableMapOf("type" to "chip", "task_id" to "t")
            if (v != null) m["located"] = v
            (FcmEvent.parse(m) as FcmEvent.New).chip.located
        }
        assertTrue(located("true"))
        assertEquals(false, located("false"))
        assertEquals(false, located(null))
    }

    @Test
    fun parsesResultAndCancel() {
        assertEquals(
            FcmEvent.Result("t", "start", ok = false, error = "agent_timeout"),
            FcmEvent.parse(mapOf("type" to "chip_result", "task_id" to "t", "action" to "start", "ok" to "false", "error" to "agent_timeout")),
        )
        assertEquals(
            FcmEvent.Result("t", "dismiss", ok = true, error = ""),
            FcmEvent.parse(mapOf("type" to "chip_result", "task_id" to "t", "action" to "dismiss", "ok" to "true")),
        )
        assertEquals(FcmEvent.Cancel("t"), FcmEvent.parse(mapOf("type" to "chip_cancel", "task_id" to "t")))
    }

    @Test
    fun ignoresUnknownOrIncomplete() {
        assertNull(FcmEvent.parse(mapOf("type" to "other", "task_id" to "t")))
        assertNull(FcmEvent.parse(mapOf("type" to "chip")))
        assertNull(FcmEvent.parse(mapOf("type" to "chip_cancel", "task_id" to "")))
    }

    @Test
    fun notificationIdIsJavaStringHashCode() {
        // Worker 側や将来の再実装と ID を揃えるため、値そのものを固定しておく
        assertEquals(-451785003, notificationId("task_12d25b98"))
        assertEquals(notificationId("task_12d25b98"), "task_12d25b98".hashCode())
    }

    @Test
    fun parsesChipList() {
        val chips = parseChips(
            """{"chips":[
              {"task_id":"task_a","title":"A","tldr":"x","cwd":"/c","host":"h","session_id":null,
               "status":"notified","located":true,"created_at":1,"updated_at":2,"error":null},
              {"task_id":"task_b","title":"B","tldr":"","cwd":"","host":"w","session_id":"s",
               "status":"failed","located":false,"created_at":1,"updated_at":2,"error":"agent_timeout"}
            ]}"""
        )
        assertEquals(2, chips.size)
        assertEquals(Chip("task_a", "A", "x", "/c", "h", "notified", true, null), chips[0])
        assertEquals("agent_timeout", chips[1].error)
        assertEquals(false, chips[1].located)
        assertEquals(emptyList<Chip>(), parseChips("""{"chips":[]}"""))
    }

    @Test
    fun parsesErrorCode() {
        assertEquals("agent_offline", parseErrorCode("""{"error":"agent_offline"}"""))
        assertNull(parseErrorCode("not json"))
        assertNull(parseErrorCode("{}"))
    }
}
