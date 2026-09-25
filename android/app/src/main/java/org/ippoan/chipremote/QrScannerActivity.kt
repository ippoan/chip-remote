package org.ippoan.chipremote

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.widget.FrameLayout
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.camera.core.CameraSelector
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors

/**
 * PC の agent が出す「スマホ接続用 QR」を読む (CameraX + ML Kit。AlcoholChecker の QrScannerActivity と同じ方式)。
 * 接続コードとして読めた QR だけを EXTRA_RESULT で返す。関係ない QR は画面に出して読み続ける。
 * 中身は secret を含むのでログに出さない。
 */
class QrScannerActivity : AppCompatActivity() {

    companion object {
        private const val TAG = "QrScanner"
        const val EXTRA_RESULT = "qr_result"
        /** RESULT_CANCELED のときの理由 (画面に出す文言)。戻るボタンで閉じたときは付かない。 */
        const val EXTRA_ERROR = "qr_error"
        private const val GUIDE = "PC の「スマホ接続用 QR」をカメラに向けてください"
    }

    @Volatile
    private var scanned = false
    private val cameraExecutor = Executors.newSingleThreadExecutor()
    private val scanner = BarcodeScanning.getClient(
        BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build()
    )
    private lateinit var previewView: PreviewView
    private lateinit var guide: TextView

    private val cameraPermission = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) startCamera() else finishWithError("カメラが許可されていません (設定から許可するか、接続コードを貼り付けてください)")
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val root = FrameLayout(this)
        previewView = PreviewView(this).apply {
            layoutParams = FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT,
            )
        }
        root.addView(previewView)

        guide = TextView(this).apply {
            text = GUIDE
            setTextColor(0xFFFFFFFF.toInt())
            textSize = 16f
            gravity = Gravity.CENTER
            setPadding(24, 48, 24, 24)
            layoutParams = FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT,
                Gravity.TOP,
            )
            setBackgroundColor(0x88000000.toInt())
        }
        root.addView(guide)
        setContentView(root)

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            startCamera()
        } else {
            cameraPermission.launch(Manifest.permission.CAMERA)
        }
    }

    @androidx.annotation.OptIn(ExperimentalGetImage::class)
    private fun startCamera() {
        val providerFuture = ProcessCameraProvider.getInstance(this)
        providerFuture.addListener({
            val provider = try {
                providerFuture.get()
            } catch (e: Exception) {
                Log.e(TAG, "camera provider unavailable", e)
                finishWithError("カメラを起動できませんでした")
                return@addListener
            }

            val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()

            analysis.setAnalyzer(cameraExecutor) { imageProxy ->
                val mediaImage = imageProxy.image
                if (mediaImage == null || scanned) {
                    imageProxy.close()
                    return@setAnalyzer
                }
                val image = InputImage.fromMediaImage(mediaImage, imageProxy.imageInfo.rotationDegrees)
                scanner.process(image)
                    .addOnSuccessListener { barcodes -> onBarcodes(barcodes) }
                    .addOnCompleteListener { imageProxy.close() }
            }

            try {
                provider.unbindAll()
                provider.bindToLifecycle(this, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
            } catch (e: Exception) {
                Log.e(TAG, "camera bind failed", e)
                finishWithError("カメラを起動できませんでした")
            }
        }, ContextCompat.getMainExecutor(this))
    }

    /** main スレッド (ML Kit の listener) で呼ばれる。 */
    private fun onBarcodes(barcodes: List<Barcode>) {
        if (scanned) return
        val values = barcodes.mapNotNull { it.rawValue }
        if (values.isEmpty()) return
        val code = values.firstOrNull { parseConnectCode(it) != null }
        if (code == null) {
            guide.text = "接続コードではありません\n$GUIDE"
            return
        }
        scanned = true
        Log.i(TAG, "connect code scanned")
        setResult(RESULT_OK, Intent().putExtra(EXTRA_RESULT, code))
        finish()
    }

    private fun finishWithError(message: String) {
        setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_ERROR, message))
        finish()
    }

    override fun onDestroy() {
        super.onDestroy()
        cameraExecutor.shutdown()
        scanner.close()
    }
}
