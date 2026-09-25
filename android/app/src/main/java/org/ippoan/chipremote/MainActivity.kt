package org.ippoan.chipremote

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.LayoutInflater
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.lifecycle.lifecycleScope
import androidx.swiperefreshlayout.widget.SwipeRefreshLayout
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

class MainActivity : AppCompatActivity() {

    private lateinit var urlInput: EditText
    private lateinit var tokenInput: EditText
    private lateinit var statusText: TextView
    private lateinit var emptyText: TextView
    private lateinit var chipList: LinearLayout
    private lateinit var swipe: SwipeRefreshLayout

    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (!granted) setStatus("通知が許可されていません (設定から許可してください)")
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        urlInput = findViewById(R.id.url_input)
        tokenInput = findViewById(R.id.token_input)
        statusText = findViewById(R.id.status_text)
        emptyText = findViewById(R.id.empty_text)
        chipList = findViewById(R.id.chip_list)
        swipe = findViewById(R.id.swipe)

        urlInput.setText(Settings.url(this))
        tokenInput.setText(Settings.token(this))

        findViewById<TextView>(R.id.firebase_text).text =
            if (ChipRemoteApp.firebaseReady) "Firebase: 設定済み (${BuildConfig.FIREBASE_PROJECT_ID})"
            else "Firebase 未設定 (通知は届きません。一覧からの操作のみ可能)"

        findViewById<Button>(R.id.register_button).setOnClickListener { register() }
        findViewById<Button>(R.id.refresh_button).setOnClickListener { refresh() }
        swipe.setOnRefreshListener { refresh() }

        requestNotificationPermission()
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    private fun saveSettings(): ApiClient? {
        Settings.save(this, urlInput.text.toString(), tokenInput.text.toString())
        return Settings.client(this).also {
            if (it == null) setStatus("Worker URL と token を入力してください")
        }
    }

    private fun register() {
        val client = saveSettings() ?: return
        if (!ChipRemoteApp.firebaseReady) {
            setStatus("Firebase 未設定のため登録できません (設定は保存しました)")
            return
        }
        setStatus("登録中…")
        lifecycleScope.launch {
            try {
                client.registerDevice(fcmToken(), Build.MODEL)
                setStatus("登録しました (${Build.MODEL})")
            } catch (e: Exception) {
                setStatus("登録失敗: ${e.message ?: e.javaClass.simpleName}")
            }
        }
    }

    private fun refresh() {
        val client = Settings.client(this)
        if (client == null) {
            swipe.isRefreshing = false
            showChips(emptyList())
            return
        }
        swipe.isRefreshing = true
        lifecycleScope.launch {
            try {
                showChips(client.openChips())
            } catch (e: Exception) {
                setStatus("一覧の取得に失敗: ${e.message ?: e.javaClass.simpleName}")
            } finally {
                swipe.isRefreshing = false
            }
        }
    }

    private fun showChips(chips: List<Chip>) {
        chipList.removeAllViews()
        emptyText.visibility = if (chips.isEmpty()) View.VISIBLE else View.GONE
        val inflater = LayoutInflater.from(this)
        for (chip in chips) {
            val row = inflater.inflate(R.layout.item_chip, chipList, false)
            row.findViewById<TextView>(R.id.chip_title).text = chip.title.ifEmpty { chip.taskId }
            row.findViewById<TextView>(R.id.chip_tldr).text = chip.tldr
            row.findViewById<TextView>(R.id.chip_meta).text = buildString {
                append(listOf(chip.host, chip.cwd).filter { it.isNotEmpty() }.joinToString(" · "))
                append("\n状態: ").append(chip.status)
                if (!chip.located) append(" (画面で未確認)")
                chip.error?.let { append(" / ").append(errorLabel(it)) }
            }
            row.findViewById<Button>(R.id.start_button).setOnClickListener { act(chip, ActionReceiver.ACTION_START) }
            row.findViewById<Button>(R.id.dismiss_button).setOnClickListener { act(chip, ActionReceiver.ACTION_DISMISS) }
            chipList.addView(row)
        }
    }

    private fun act(chip: Chip, action: String) {
        val client = saveSettings() ?: return
        setStatus("送信中… (${chip.title})")
        lifecycleScope.launch {
            when (val r = client.postAction(chip.taskId, action)) {
                ActionOutcome.Accepted -> {
                    setStatus("Windows に送信しました (結果は通知で届きます)")
                    Notifier.showAccepted(this@MainActivity, chip.toNotice())
                }
                ActionOutcome.AgentOffline -> setStatus(errorLabel("agent_offline"))
                ActionOutcome.ChipClosed -> {
                    setStatus(errorLabel("chip_closed"))
                    Notifier.cancel(this@MainActivity, chip.taskId)
                }
                is ActionOutcome.Failed -> setStatus(r.message)
            }
            refresh()
        }
    }

    private fun setStatus(text: String) {
        statusText.text = text
    }

    private suspend fun fcmToken(): String = suspendCancellableCoroutine { cont ->
        FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
            if (task.isSuccessful) cont.resume(task.result)
            else cont.resumeWithException(task.exception ?: IllegalStateException("FCM token unavailable"))
        }
    }
}
