package com.mehrnet.bamdad

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
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

        // Capture the physical network before the VPN becomes active.
        val connectivity = getSystemService(ConnectivityManager::class.java)
        val underlying = connectivity?.activeNetwork

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
            stopTunnel()
            stopSelf()
            return
        }

        // Give the tunnel an explicit underlying network. Without this some
        // devices leave the VPN without a working uplink.
        try {
            if (underlying != null) {
                setUnderlyingNetworks(arrayOf(underlying))
            } else {
                setUnderlyingNetworks(null)
            }
            Log.i(TAG, "underlying network: $underlying")
        } catch (error: Throwable) {
            Log.w(TAG, "setUnderlyingNetworks failed: ${error.message}")
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
                TProxyService.TProxyStopService()
            } catch (error: Throwable) {
                Log.e(TAG, "hev stop failed: ${error.message}")
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

        @Volatile
        private var instance: BamdadVpnService? = null

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
