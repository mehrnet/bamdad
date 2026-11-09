package com.mehrnet.bamdad

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import hev.htproxy.TProxyService
import java.io.File

/**
 * Android has no general app-level proxy API. To route other apps through the
 * selected core we establish a VpnService TUN and forward it to the core's
 * local SOCKS listener with hev-socks5-tunnel.
 */
class BamdadVpnService : VpnService() {
    private var tunFd: ParcelFileDescriptor? = null
    private var socksPort: Int = 10808
    private var running = false

    override fun onCreate() {
        super.onCreate()
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopTunnel()
            stopSelf()
            return START_NOT_STICKY
        }

        socksPort = intent?.getIntExtra(EXTRA_SOCKS_PORT, 10808) ?: 10808
        startForegroundNotification()
        if (!running) {
            startTunnel()
        }
        return START_STICKY
    }

    private fun startTunnel() {
        if (tunFd != null) return

        val builder = Builder()
            .setSession("Bamdad")
            .setMtu(MTU)
            .setBlocking(false)
            .addAddress("172.19.0.1", 30)
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
            stopSelf()
            return
        }

        val config = File(cacheDir, "hev-socks5-tunnel.yml")
        config.writeText(
            """
            misc:
              task-stack-size: 20480
            tunnel:
              mtu: $MTU
            socks5:
              port: $socksPort
              address: '127.0.0.1'
              udp: 'udp'
            """.trimIndent() + "\n"
        )

        if (!TProxyService.TProxyStartService(config.absolutePath, fd.fd)) {
            Log.e(TAG, "hev-socks5-tunnel failed to start")
            fd.close()
            stopSelf()
            return
        }

        tunFd = fd
        running = true
        Log.i(TAG, "VPN tunnel started (socks 127.0.0.1:$socksPort)")
    }

    private fun stopTunnel() {
        if (running) {
            TProxyService.TProxyStopService()
            running = false
        }
        tunFd?.close()
        tunFd = null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
        Log.i(TAG, "VPN tunnel stopped")
    }

    override fun onRevoke() {
        stopTunnel()
        stopSelf()
        super.onRevoke()
    }

    override fun onDestroy() {
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

    private fun startForegroundNotification() {
        val intent = Intent(this, MainActivity::class.java)
        val pending = PendingIntent.getActivity(
            this,
            0,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification: Notification = Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle("Bamdad VPN")
            .setContentText("Traffic is routed through Bamdad")
            .setContentIntent(pending)
            .setOngoing(true)
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

    companion object {
        private const val TAG = "BamdadVpn"
        private const val CHANNEL_ID = "bamdad_vpn"
        private const val NOTIFICATION_ID = 2001
        private const val MTU = 1500

        const val ACTION_STOP = "com.mehrnet.bamdad.VPN_STOP"
        const val EXTRA_SOCKS_PORT = "socks_port"

        fun start(context: Context, socksPort: Int) {
            val intent = Intent(context, BamdadVpnService::class.java)
                .putExtra(EXTRA_SOCKS_PORT, socksPort)
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, BamdadVpnService::class.java))
        }
    }
}
