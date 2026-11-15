package com.mehrnet.bamdad

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
        BamdadNotification.ensureChannel(this)
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
        BamdadNotification.startForeground(
            this,
            BamdadNotification.KILL_SWITCH_ID,
            "Bamdad",
            "Traffic blocked",
            "Kill switch",
            "orange",
        )
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
