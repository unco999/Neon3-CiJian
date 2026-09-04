package com.neon3.androidruntime;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;

/**
 * Invisible launcher. Starts the Neon3 headless host service (no window) and
 * finishes immediately so the runtime runs in the background. A rendered
 * surface is created only when an SDK explicitly requests it.
 */
public final class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        try {
            Intent service = new Intent(this, Neon3HostService.class);
            if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
                startForegroundService(service);
            } else {
                startService(service);
            }
            Log.i("Neon3Probe", "{\"probe\":\"android-launcher\",\"state\":\"service_started\"}");
        } catch (RuntimeException error) {
            Log.e("Neon3Probe", "{\"probe\":\"android-launcher\",\"state\":\"failed\",\"error\":\""
                    + error.getMessage() + "\"}");
        }
        finish();
    }
}