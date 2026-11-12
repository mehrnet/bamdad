package com.mehrnet.bamdad

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.PorterDuff
import android.graphics.drawable.Icon
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log

/**
 * Android has no general app-level proxy API. To route other apps through the
 * selected core we establish a VpnService TUN and forward it to the core's
 * local SOCKS listener with the bundled tun2socks process.
 */
class BamdadVpnService : VpnService() {
    private var tunFd: ParcelFileDescriptor? = null
    private var socksPort: Int = 10808
    private var running = false

    override fun onCreate() {
        super.onCreate()
        instance = this
        createChannel()
        Log.i(TAG, "service created")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            Log.i(TAG, "stop action received")
            stopTunnel()
            stopSelf()
            return START_NOT_STICKY
        }

        socksPort = intent?.getIntExtra(EXTRA_SOCKS_PORT, 10808) ?: 10808
        startForegroundNotification()
        if (!running) {
            startTunnel()
        }
        // Do not resurrect a VPN after the user explicitly disconnects.
        return START_NOT_STICKY
    }

    private fun startTunnel() {
        if (tunFd != null) return

        // IPv4-only by default: many mobile networks advertise IPv6 but the
        // proxy egress cannot reach it, which makes apps hang on AAAA records.
        val builder = Builder()
            .setSession("Bamdad")
            .setMtu(MTU)
            .setBlocking(false)
            .addAddress("172.19.0.1", 30)
            // Capture IPv6 as well. Even if the proxy has no IPv6 egress,
            // capturing it prevents apps from leaking over the physical link.
            .addAddress("fdfe:dcba:9876::1", 126)
            .addRoute("0.0.0.0", 0)
            .addRoute("::", 0)
            .addDnsServer("1.1.1.1")
            .addDnsServer("8.8.8.8")
            .addDnsServer("2606:4700:4700::1111")

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            builder.setMetered(false)
        }
        try {
            // The core runs under the same UID and must use the physical
            // network, not loop back into the tunnel.
            builder.addDisallowedApplication(packageName)
        } catch (error: Exception) {
            Log.w(TAG, "could not exclude $packageName: ${error.message}")
        }

        val fd = try {
            builder.establish()
        } catch (error: Exception) {
            Log.e(TAG, "establish failed", error)
            null
        }
        if (fd == null) {
            Log.e(TAG, "VpnService.Builder.establish() returned null")
            stopTunnel()
            stopSelf()
            return
        }

        if (!MainActivity.startTunnel(fd.fd, socksPort)) {
            Log.e(TAG, "tun2socks failed to start")
            fd.close()
            stopTunnel()
            stopSelf()
            return
        }

        tunFd = fd
        running = true
        Log.i(TAG, "VPN tunnel started (socks 127.0.0.1:$socksPort)")
    }

    @Synchronized
    private fun stopTunnel() {
        Log.i(TAG, "stopping tunnel")
        if (running) {
            try {
                MainActivity.stopTunnel()
            } catch (error: Throwable) {
                Log.e(TAG, "tun2socks stop failed: ${error.message}")
            }
            running = false
        }
        try {
            tunFd?.close()
        } catch (error: Throwable) {
            Log.w(TAG, "tun fd close failed: ${error.message}")
        }
        tunFd = null
        try {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } catch (error: Throwable) {
            Log.w(TAG, "stopForeground failed: ${error.message}")
        }
        Log.i(TAG, "VPN tunnel stopped")
    }

    override fun onRevoke() {
        Log.i(TAG, "VPN revoked by system")
        stopTunnel()
        stopSelf()
        super.onRevoke()
    }

    override fun onDestroy() {
        Log.i(TAG, "service destroyed")
        instance = null
        stopTunnel()
        super.onDestroy()
    }

    private fun createChannel() {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID,
            "VPN",
            NotificationManager.IMPORTANCE_LOW,
        ).apply { description = "Bamdad VPN status" }
        manager.createNotificationChannel(channel)
    }

    private fun buildNotification(): Notification {
        val intent = Intent(this, MainActivity::class.java)
        val pending = PendingIntent.getActivity(
            this,
            0,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val title = activeProfile.ifBlank { "Bamdad VPN" }
        val traffic = "↓ $downText   ↑ $upText"
        val accent = accentColor(activeAccent)
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(Icon.createWithBitmap(accentIcon(accent)))
            .setColor(accent)
            .setContentTitle(title)
            .setContentText(traffic)
            .setContentIntent(pending)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
    }

    private fun startForegroundNotification() {
        val notification = buildNotification()
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

    private fun accentIcon(color: Int): Bitmap {
        val density = resources.displayMetrics.density
        val size = (24 * density).toInt().coerceAtLeast(24)
        val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        val drawable = getDrawable(R.drawable.ic_moon)
        if (drawable != null) {
            drawable.setBounds(0, 0, size, size)
            drawable.setColorFilter(color, PorterDuff.Mode.SRC_IN)
            drawable.draw(canvas)
            drawable.setColorFilter(null)
        }
        return bitmap
    }

    private fun refreshNotification() {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        try {
            manager.notify(NOTIFICATION_ID, buildNotification())
        } catch (error: Throwable) {
            Log.w(TAG, "notification update failed: ${error.message}")
        }
    }

    companion object {
        private const val TAG = "BamdadVpn"
        private const val CHANNEL_ID = "bamdad_vpn"
        private const val NOTIFICATION_ID = 2001
        private const val MTU = 1400

        const val ACTION_STOP = "com.mehrnet.bamdad.VPN_STOP"
        const val EXTRA_SOCKS_PORT = "socks_port"

        @Volatile
        private var instance: BamdadVpnService? = null
        @Volatile
        private var activeProfile: String = ""
        @Volatile
        private var downText: String = "0 B"
        @Volatile
        private var upText: String = "0 B"
        @Volatile
        private var activeAccent: String = "cyan"

        /** Called from Rust with the selected profile and live traffic totals. */
        fun updateStats(profile: String, down: String, up: String, accent: String) {
            if (profile.isNotBlank()) {
                activeProfile = profile
            }
            downText = down
            upText = up
            activeAccent = accent
            instance?.refreshNotification()
        }

        private fun accentColor(name: String): Int = when (name) {
            "blue" -> 0xFF60A5FA.toInt()
            "red" -> 0xFFF87171.toInt()
            "green" -> 0xFF4ADE80.toInt()
            "purple" -> 0xFFA78BFA.toInt()
            "orange" -> 0xFFFB923C.toInt()
            "pink" -> 0xFFF472B6.toInt()
            "gray" -> 0xFF94A3B8.toInt()
            else -> 0xFF38BDF8.toInt()
        }

        fun start(context: Context, socksPort: Int) {
            Log.i(TAG, "start requested (socks $socksPort)")
            val intent = Intent(context, BamdadVpnService::class.java)
                .putExtra(EXTRA_SOCKS_PORT, socksPort)
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            Log.i(TAG, "stop requested")
            instance?.let { service ->
                service.stopTunnel()
                service.stopSelf()
            }
            context.stopService(Intent(context, BamdadVpnService::class.java))
        }
    }
}
