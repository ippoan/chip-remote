import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// 設定値の解決順: 環境変数 > gradle property (-P / ~/.gradle/gradle.properties) > android/local.properties
// google-services プラグインは使わない (public リポなので google-services.json を置けない、CI は secret 無しでも通す)。
val localProps = Properties().apply {
    val f = rootProject.file("local.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}

fun cfg(prop: String, env: String, default: String = ""): String =
    System.getenv(env)?.takeIf { it.isNotBlank() }
        ?: (findProperty(prop) as String?)?.takeIf { it.isNotBlank() }
        ?: localProps.getProperty(prop)?.takeIf { it.isNotBlank() }
        ?: default

fun quoted(s: String) = "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\""

// 署名: ANDROID_KEYSTORE_FILE が指す keystore があるときだけ release を署名する (CI が base64 から復元する)。
val keystorePath = cfg("chipremote.keystore.file", "ANDROID_KEYSTORE_FILE")
val hasKeystore = keystorePath.isNotEmpty() && file(keystorePath).exists()

android {
    namespace = "org.ippoan.chipremote"
    compileSdk = 34

    defaultConfig {
        applicationId = "org.ippoan.chipremote"
        minSdk = 26
        targetSdk = 34
        versionCode = (System.getenv("GITHUB_RUN_NUMBER") ?: "1").toInt()
        versionName = "0.1.${versionCode}"

        buildConfigField("String", "FIREBASE_APP_ID", quoted(cfg("chipremote.firebase.appId", "FIREBASE_APP_ID")))
        buildConfigField("String", "FIREBASE_API_KEY", quoted(cfg("chipremote.firebase.apiKey", "FIREBASE_API_KEY")))
        buildConfigField("String", "FIREBASE_PROJECT_ID", quoted(cfg("chipremote.firebase.projectId", "FIREBASE_PROJECT_ID", "alc-fcm")))
        buildConfigField("String", "FIREBASE_SENDER_ID", quoted(cfg("chipremote.firebase.senderId", "FIREBASE_SENDER_ID")))
    }

    signingConfigs {
        if (hasKeystore) {
            create("release") {
                storeFile = file(keystorePath)
                storePassword = cfg("chipremote.keystore.password", "ANDROID_KEYSTORE_PASSWORD")
                keyAlias = cfg("chipremote.key.alias", "ANDROID_KEY_ALIAS")
                keyPassword = cfg("chipremote.key.password", "ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (hasKeystore) signingConfig = signingConfigs.getByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        buildConfig = true
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.appcompat:appcompat:1.6.1")
    implementation("com.google.android.material:material:1.11.0")
    implementation("androidx.swiperefreshlayout:swiperefreshlayout:1.1.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.7.3")

    implementation(platform("com.google.firebase:firebase-bom:33.7.0"))
    implementation("com.google.firebase:firebase-messaging")

    testImplementation("junit:junit:4.13.2")
    // android.jar の org.json はスタブなので、JVM テストでは本物を使う
    testImplementation("org.json:json:20240303")
}
