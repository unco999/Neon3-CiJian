package com.neon3.androidruntime;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.os.IBinder;
import android.util.Log;

/**
 * Foreground host service. Loads the native Neon3 host and runs the headless
 * protocol endpoint in the background. No window or WGPU surface is created
 * here; an SDK opens a rendered surface through the public protocol.
 */
public final class Neon3HostService extends Service {
    private static final String CHANNEL_ID = "neon3-host";
    private static final String ENDPOINT = "127.0.0.1:43100";
    private boolean nativeStarted = false;
    private static volatile Neon3HostService active;

    static {
        try {
            System.loadLibrary("neon_android_host");
        } catch (UnsatisfiedLinkError error) {
            Log.e("Neon3Probe", "neon_android_host load failed: " + error.getMessage());
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        active = this;
        startForegroundCompat();
        int result = hostStart(ENDPOINT);
        nativeStarted = result == 0;
        Log.i("Neon3Probe", "{\"probe\":\"android-host-service\",\"state\":\"started\""
                + ",\"result\":" + result + ",\"endpoint\":\"" + ENDPOINT + "\"}");
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // The SDK is the lifecycle owner: when it sends service.shutdown the
        // host stops itself and Android must NOT resurrect the process.
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        if (nativeStarted) {
            hostStop(ENDPOINT);
            nativeStarted = false;
        }
        active = null;
        stopForeground(true);
        Log.i("Neon3Probe", "{\"probe\":\"android-host-service\",\"state\":\"stopped\"}");
        super.onDestroy();
    }

    /**
     * Invoked from the native headless host thread when the protocol server
     * exits (service.shutdown or failure). Stops this foreground service so
     * the process can exit cleanly when the SDK is done.
     */
    @SuppressWarnings("unused")
    public static void onHostServerStopped() {
        Neon3HostService service = active;
        if (service != null) {
            service.stopSelf();
        }
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void startForegroundCompat() {
        NotificationManager manager =
                (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
            NotificationChannel channel =
                    new NotificationChannel(CHANNEL_ID, "Neon3 Host", NotificationManager.IMPORTANCE_MIN);
            channel.setShowBadge(false);
            manager.createNotificationChannel(channel);
        }
        Notification notification;
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
            notification = new Notification.Builder(this, CHANNEL_ID)
                    .setContentTitle("Neon3 Host")
                    .setContentText("Headless runtime is running")
                    .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                    .build();
        } else {
            notification = new Notification.Builder(this)
                    .setContentTitle("Neon3 Host")
                    .setContentText("Headless runtime is running")
                    .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                    .build();
        }
        startForeground(1, notification);
    }

    private static native int hostStart(String endpoint);
    private static native void hostStop(String endpoint);
}