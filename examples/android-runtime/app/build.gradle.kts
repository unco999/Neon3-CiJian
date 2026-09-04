plugins {
    id("com.android.application")
}

android {
    namespace = "com.neon3.androidruntime"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.neon3.androidruntime"
        minSdk = 29
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
}
