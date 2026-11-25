package com.mehrnet.bamdad

import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import java.net.InetAddress

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
        BamdadNotification.ensureChannel(this)
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
        for (route in TunnelOptions.bypassRoutes("0.0.0.0/0")) {
            builder.addRoute(InetAddress.getByName(route.address), route.prefix)
        }
        for (route in TunnelOptions.bypassRoutes("::/0")) {
            builder.addRoute(InetAddress.getByName(route.address), route.prefix)
        }
        if (TunnelOptions.dns.isEmpty()) {
            builder.addDnsServer("1.1.1.1")
            builder.addDnsServer("8.8.8.8")
            builder.addDnsServer("2606:4700:4700::1111")
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            builder.setMetered(false)
        }
        // The core runs under the same UID and must use the physical network,
        // not loop back into the tunnel. In "only these apps" mode the builder
        // cannot mix allowed and disallowed applications - and it does not
        // need to: Bamdad is simply not on the allow list, so it stays
        // outside. Calling addDisallowedApplication first made every
        // addAllowedApplication throw and silently disabled split tunneling.
        val allowOnly = TunnelOptions.splitMode == "allow" && TunnelOptions.splitApps.isNotEmpty()
        if (!allowOnly) {
            try {
                builder.addDisallowedApplication(packageName)
            } catch (error: Exception) {
                Log.w(TAG, "could not exclude $packageName: ${error.message}")
            }
        }
        TunnelOptions.apply(builder, packageName)

        val fd = try {
            builder.establish()
        } catch (error: Exception) {
            Log.e(TAG, "establish failed", error)
            null
        }
        if (fd == null) {
            Log.e(TAG, "VpnService.Builder.establish() returned null")
            MainActivity.nativeProxyError("Android rejected the VPN interface")
            stopTunnel()
            stopSelf()
            return
        }

        if (!MainActivity.startTunnel(fd.fd, socksPort)) {
            Log.e(TAG, "tun2socks failed to start")
            fd.close()
            stopTunnel()
            stopSelf()
            MainActivity.nativeProxyError("Could not start the traffic forwarder")
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

    private fun notificationText(): String = "↓ $downText   ↑ $upText"

    private fun startForegroundNotification() {
        BamdadNotification.startForeground(
            this,
            BamdadNotification.TUNNEL_ID,
            activeProfile,
            notificationText(),
            "Proxy",
            activeAccent,
        )
    }

    private fun refreshNotification() {
        BamdadNotification.update(
            this,
            BamdadNotification.TUNNEL_ID,
            activeProfile,
            notificationText(),
            "Proxy",
            activeAccent,
        )
    }

    companion object {
        private const val TAG = "BamdadVpn"
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

        /** True once the service is gone and the VPN slot is free. */
        fun released(): Boolean = instance == null

        fun start(context: Context, socksPort: Int) {
            Log.i(TAG, "start requested (socks $socksPort)")
            val intent = Intent(context, BamdadVpnService::class.java)
                .putExtra(EXTRA_SOCKS_PORT, socksPort)
            try {
                context.startForegroundService(intent)
            } catch (error: Throwable) {
                // Android 12+ rejects a foreground service started from the
                // background; without this the process simply crashed.
                Log.e(TAG, "could not start the VPN service", error)
                MainActivity.nativeProxyError(
                    "Could not start the VPN service: ${error.message}",
                )
            }
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
