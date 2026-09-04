package com.neon3.androidruntime;

final class NeonRuntimeBridge {
    static {
        try {
            System.loadLibrary("neon_android_host");
        } catch (UnsatisfiedLinkError ignored) {
            // The APK remains diagnosable if the native library is absent.
        }
    }

    private NeonRuntimeBridge() {}

    private static native String capabilitiesJson();

    static String capabilitiesOrFallback() {
        try {
            return capabilitiesJson();
        } catch (UnsatisfiedLinkError ignored) {
            return "{\"platform\":\"android\",\"runtime_mode\":\"embedded_host\",\"native\":false}";
        }
    }
}
