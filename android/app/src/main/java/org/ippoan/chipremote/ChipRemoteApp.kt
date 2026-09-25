package org.ippoan.chipremote

import android.app.Application
import android.util.Log
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions

class ChipRemoteApp : Application() {

    override fun onCreate() {
        super.onCreate()
        firebaseReady = initFirebase()
        Notifier.ensureChannel(this)
        UpdateChecker.ensureChannel(this)
        // 初回登録時にすぐ 1 回走り、以後 6 時間ごと (アプリを開かなくても更新通知が出る)
        UpdateChecker.schedule(this)
    }

    /** BuildConfig の値から FirebaseApp を手動で作る。値が欠けていれば false (FCM 無しで動く)。 */
    private fun initFirebase(): Boolean {
        val appId = BuildConfig.FIREBASE_APP_ID
        val apiKey = BuildConfig.FIREBASE_API_KEY
        val senderId = BuildConfig.FIREBASE_SENDER_ID
        if (appId.isBlank() || apiKey.isBlank() || senderId.isBlank()) {
            Log.w(TAG, "Firebase not configured; FCM disabled")
            return false
        }
        if (FirebaseApp.getApps(this).isNotEmpty()) return true
        return try {
            val options = FirebaseOptions.Builder()
                .setApplicationId(appId)
                .setApiKey(apiKey)
                .setProjectId(BuildConfig.FIREBASE_PROJECT_ID)
                .setGcmSenderId(senderId)
                .build()
            FirebaseApp.initializeApp(this, options)
            true
        } catch (e: Exception) {
            Log.e(TAG, "Firebase init failed", e)
            false
        }
    }

    companion object {
        private const val TAG = "ChipRemoteApp"

        /** Firebase を初期化できたか。false なら FCM 関連の呼び出しをしない。 */
        @Volatile
        var firebaseReady: Boolean = false
            private set
    }
}
