plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "app.modelroamer"
    compileSdk = 34

    defaultConfig {
        applicationId = "app.modelroamer"
        minSdk = 24
        targetSdk = 34
        versionCode = (System.getenv("MR_VERSION_CODE") ?: "1").toInt()
        versionName = System.getenv("MR_VERSION_NAME") ?: "0.1.0"
    }

    buildTypes {
        // Installed by hand (sideloaded), so it is signed with the debug key and needs no store account.
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}
