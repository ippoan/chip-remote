package org.ippoan.chipremote

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.view.LayoutInflater
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import androidx.swiperefreshlayout.widget.SwipeRefreshLayout
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.delay
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
    private lateinit var settingsGroup: View
    private lateinit var settingsToggle: Button
    private lateinit var updateButton: Button
    private lateinit var updateStatus: TextView

    /** 確認済みの新しい版 (無ければ null)。ボタンが「vX に更新」になる。 */
    private var availableUpdate: UpdateInfo? = null
    /** 「更新を確認」の結果など、Updater が Idle のときに出す文言。 */
    private var updateMessage: String = ""
    /** 「不明なアプリのインストール」の設定画面から戻ったら更新を続ける。 */
    private var resumeUpdateAfterSettings = false

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

        settingsGroup = findViewById(R.id.settings_group)
        settingsToggle = findViewById(R.id.settings_toggle)
        settingsToggle.setOnClickListener { setSettingsOpen(settingsGroup.visibility != View.VISIBLE) }
        // token が保存済みなら接続設定は閉じておく (普段は一覧だけ見えればよい)
        setSettingsOpen(Settings.token(this).isBlank())

        findViewById<TextView>(R.id.firebase_text).text =
            if (ChipRemoteApp.firebaseReady) "Firebase: 設定済み (${BuildConfig.FIREBASE_PROJECT_ID})"
            else "Firebase 未設定 (通知は届きません。一覧からの操作のみ可能)"

        findViewById<Button>(R.id.register_button).setOnClickListener { register() }
        findViewById<Button>(R.id.refresh_button).setOnClickListener { refresh() }
        swipe.setOnRefreshListener { refresh() }

        updateButton = findViewById(R.id.update_button)
        updateStatus = findViewById(R.id.update_status)
        findViewById<TextView>(R.id.version_text).text =
            "v${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})"
        updateButton.setOnClickListener {
            val info = availableUpdate
            if (info != null) startUpdate(info) else checkUpdate(manual = true)
        }
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                Updater.state.collect { renderUpdate(it) }
            }
        }

        requestNotificationPermission()
        if (savedInstanceState == null) handleUpdateIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleUpdateIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        Updater.foreground = true
        refresh()
        if (resumeUpdateAfterSettings) {
            resumeUpdateAfterSettings = false
            availableUpdate?.takeIf { packageManager.canRequestPackageInstalls() }?.let { startUpdate(it) }
        } else if (System.currentTimeMillis() - lastAutoCheckAt > AUTO_CHECK_INTERVAL_MS) {
            lastAutoCheckAt = System.currentTimeMillis()
            checkUpdate(manual = false)
        }
    }

    override fun onPause() {
        super.onPause()
        Updater.foreground = false
    }

    /** 更新通知のタップ: 最新の version.json を取り直してそのまま更新に進む。 */
    private fun handleUpdateIntent(intent: Intent?) {
        if (intent?.getBooleanExtra(EXTRA_START_UPDATE, false) != true) return
        intent.removeExtra(EXTRA_START_UPDATE)
        UpdateChecker.cancelNotification(this)
        lifecycleScope.launch {
            val info = fetchUpdate() ?: return@launch
            if (isNewer(info, BuildConfig.VERSION_CODE)) startUpdate(info) else setUpdateMessage("最新です")
        }
    }

    /** manual = false (起動時) は通知の 1 日 1 回制限つき。どちらも結果を画面に出す。 */
    private fun checkUpdate(manual: Boolean) {
        if (Updater.busy) return
        if (manual) setUpdateMessage("更新を確認中…")
        lifecycleScope.launch {
            val info = if (manual) {
                // 失敗時は fetchUpdate が画面に出している
                val latest = fetchUpdate() ?: return@launch
                latest.takeIf { isNewer(it, BuildConfig.VERSION_CODE) }
            } else {
                try {
                    UpdateChecker.checkAndNotify(this@MainActivity)
                } catch (e: Exception) {
                    // 起動時の確認は黙って失敗する (オフラインなど)
                    UpdateChecker.logFailure(e)
                    return@launch
                }
            }
            availableUpdate = info
            when {
                info != null -> setUpdateMessage("更新があります: ${info.label}")
                manual -> setUpdateMessage("最新です")
                else -> renderUpdate(Updater.state.value)
            }
        }
    }

    /** 手動確認・通知タップ用。失敗は画面に出して null。 */
    private suspend fun fetchUpdate(): UpdateInfo? = try {
        UpdateChecker.fetch()
    } catch (e: Exception) {
        setUpdateMessage("更新の確認に失敗: ${e.message ?: e.javaClass.simpleName}")
        null
    }

    private fun startUpdate(info: UpdateInfo) {
        availableUpdate = info
        if (!packageManager.canRequestPackageInstalls()) {
            AlertDialog.Builder(this)
                .setTitle("更新の許可")
                .setMessage("更新を入れるには、chip-remote に「不明なアプリのインストール」を許可してください (初回のみ)。許可したら戻ってください。")
                .setPositiveButton("設定を開く") { _, _ ->
                    resumeUpdateAfterSettings = true
                    startActivity(
                        Intent(
                            android.provider.Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                            Uri.parse("package:$packageName"),
                        )
                    )
                }
                .setNegativeButton("キャンセル", null)
                .show()
            return
        }
        Updater.start(this, info)
    }

    private fun setUpdateMessage(text: String) {
        updateMessage = text
        renderUpdate(Updater.state.value)
    }

    private fun renderUpdate(state: UpdateState) {
        val text = when (state) {
            UpdateState.Idle -> updateMessage
            is UpdateState.Downloading -> "${state.info.label} をダウンロード中… ${state.percent}%"
            is UpdateState.Installing -> "インストール画面で「更新」を押してください"
            is UpdateState.Failed -> state.message
        }
        updateStatus.text = text
        updateStatus.visibility = if (text.isEmpty()) View.GONE else View.VISIBLE
        updateButton.isEnabled = state !is UpdateState.Downloading
        updateButton.text = availableUpdate?.let { "${it.label} に更新" } ?: "更新を確認"
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
                setSettingsOpen(false)
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
        Notifier.showSending(this, chip.toNotice())
        lifecycleScope.launch {
            when (val r = client.postAction(chip.taskId, action)) {
                ActionOutcome.Accepted -> {
                    setStatus("Windows に送信しました (結果は通知で届きます)")
                    // chip_result が先に届いていたら上書きしない (Notifier 参照)
                    Notifier.showAcceptedIfStillSending(this@MainActivity, chip.toNotice())
                    // PC 側は 1 秒未満で終わるので、少し待って結果を一覧に反映する
                    delay(2_000)
                }
                ActionOutcome.AgentOffline -> {
                    setStatus(errorLabel("agent_offline"))
                    Notifier.showError(this@MainActivity, chip.toNotice(), errorLabel("agent_offline"))
                }
                ActionOutcome.ChipClosed -> {
                    setStatus(errorLabel("chip_closed"))
                    Notifier.cancel(this@MainActivity, chip.taskId)
                }
                is ActionOutcome.Failed -> {
                    setStatus(r.message)
                    Notifier.showError(this@MainActivity, chip.toNotice(), r.message)
                }
            }
            refresh()
        }
    }

    private fun setSettingsOpen(open: Boolean) {
        settingsGroup.visibility = if (open) View.VISIBLE else View.GONE
        settingsToggle.text = if (open) "接続設定を閉じる" else "接続設定を表示"
    }

    private fun setStatus(text: String) {
        statusText.text = text
    }

    companion object {
        /** 更新通知のタップで付く。true なら更新に進む。 */
        const val EXTRA_START_UPDATE = "start_update"

        /** onResume ごとの自動確認は 10 分に 1 回まで (通知自体は UpdateChecker が 1 日 1 回に絞る)。 */
        private const val AUTO_CHECK_INTERVAL_MS = 10 * 60 * 1000L
        private var lastAutoCheckAt = 0L
    }

    private suspend fun fcmToken(): String = suspendCancellableCoroutine { cont ->
        FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
            if (task.isSuccessful) cont.resume(task.result)
            else cont.resumeWithException(task.exception ?: IllegalStateException("FCM token unavailable"))
        }
    }
}
