package com.mehrnet.bamdad

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log

/**
 * No-leak sinkhole. While no tunnel owns the VPN slot, this service holds a
 * tun that captures all traffic and forwards nothing, so apps cannot fall back
 * to the physical network when the kill switch setting is enabled.
 */
class KillSwitchService : VpnService() {
    companion object {
        private const val TAG = "BamdadKillSwitch"
        private const val CHANNEL_ID = "bamdad_killswitch"
        private const val NOTIFICATION_ID = 44

        @Volatile
        private var active = false

        fun isActive(): Boolean = active

        fun start(context: Context) {
            val intent = Intent(context, KillSwitchService::class.java)
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            } catch (error: Throwable) {
                Log.w(TAG, "could not start the kill switch: ${error.message}")
            }
        }

        fun stop(context: Context) {
            runCatching {
                context.stopService(Intent(context, KillSwitchService::class.java))
            }
            active = false
        }
    }

    private var tun: ParcelFileDescriptor? = null

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Kill switch",
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = "Blocks traffic while no tunnel is active" },
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForegroundNotification()
        if (tun == null) {
            val builder = Builder()
                .setSession("Bamdad kill switch")
                .setMtu(1500)
                .setBlocking(true)
                .addAddress("10.255.255.2", 32)
                .addRoute("0.0.0.0", 0)
                .addRoute("::", 0)
            try {
                builder.addDisallowedApplication(packageName)
            } catch (_: Throwable) {
            }
            tun = try {
                builder.establish()
            } catch (error: Throwable) {
                Log.e(TAG, "establish failed", error)
                null
            }
            if (tun == null) {
                Log.e(TAG, "could not establish the sinkhole tun")
                stopSelf()
                return START_NOT_STICKY
            }
        }
        active = true
        return START_STICKY
    }

    private fun startForegroundNotification() {
        val pending = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_moon)
            .setColor(android.graphics.Color.parseColor("#06B6D4"))
            .setContentTitle("Bamdad")
            .setContentText("Kill switch · traffic blocked")
            .setContentIntent(pending)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    override fun onRevoke() {
        stopSelf()
    }

    override fun onDestroy() {
        runCatching { tun?.close() }
        tun = null
        active = false
        super.onDestroy()
    }
}
