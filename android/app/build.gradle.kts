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
        versionName = System.getenv("MR_VERSION_NAME") ?: "2.1.0"
    }

    // Installed by hand (sideloaded), so no store account is involved. One fixed key, kept in the repo (it protects
    // nothing: anyone can sideload anything), so every build can update the one before it. A key made fresh on each
    // build would make Android refuse the update.
    signingConfigs {
        create("sideload") {
            storeFile = file("sideload.keystore")
            storePassword = "android"
            keyAlias = "roamer"
            keyPassword = "android"
        }
    }
    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("sideload")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}
