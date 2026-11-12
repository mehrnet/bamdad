package com.mehrnet.bamdad

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Foreground owner for native tunnel backends. The backend itself owns the
 * Android VpnService; this service keeps the process alive and publishes the
 * transport-neutral status back to Rust.
 */
class TunnelService : Service() {
    companion object {
        private const val TAG = "BamdadTunnel"
        private const val CHANNEL_ID = "bamdad_tunnel"
        private const val NOTIFICATION_ID = 43

        const val ACTION_START = "com.mehrnet.bamdad.tunnel.START"
        const val ACTION_STOP = "com.mehrnet.bamdad.tunnel.STOP"
        const val EXTRA_PROFILE = "profile"
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var profile: TunnelProfile? = null
    private var lastStatus: TunnelStatus? = null
    private var watchdogJob: Job? = null

    private fun log(message: String) {
        Log.i(TAG, message)
        MainActivity.nativeLog("BamdadTunnel: $message")
    }

    override fun onBind(intent: Intent?): android.os.IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID,
            "VPN tunnel",
            NotificationManager.IMPORTANCE_LOW,
        ).apply { description = "Native WireGuard/AmneziaWG tunnel" }
        manager.createNotificationChannel(channel)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                watchdogJob?.cancel()
                scope.launch {
                    TunnelController.disconnect()
                    stopForegroundCompat()
                    stopSelf()
                }
                return START_NOT_STICKY
            }
            ACTION_START -> {
                val raw = intent.getStringExtra(EXTRA_PROFILE) ?: return START_NOT_STICKY
                val parsed = runCatching { TunnelProfile.fromJson(raw) }.getOrNull()
                    ?: run {
                        log("invalid native profile payload")
                        runCatching {
                            startForegroundCompat(
                                buildNotification(
                                    TunnelProfile("", "Bamdad", TunnelTransport.PROXY, ""),
                                    "Invalid profile",
                                ),
                            )
                        }
                        MainActivity.nativeTunnelEvent(
                            """{"state":"error","error":"Invalid native tunnel profile"}""",
                        )
                        stopSelf()
                        return START_NOT_STICKY
                    }
                profile = parsed
                log("service start ${parsed.name} (${parsed.transport})")
                try {
                    startForegroundCompat(buildNotification(parsed, "Connecting…"))
                } catch (error: Throwable) {
                    log("startForeground failed: ${error.message}")
                    MainActivity.nativeTunnelEvent(
                        """{"state":"error","error":"Could not start VPN notification: ${error.message}"}""",
                    )
                    stopSelf()
                    return START_NOT_STICKY
                }
                scope.launch {
                    try {
                        TunnelController.connect(this@TunnelService, parsed) { status ->
                            publish(parsed, status)
                        }
                        startHandshakeWatchdog(parsed)
                    } catch (error: Throwable) {
                        Log.e(TAG, "tunnel connect failed", error)
                        log("connect failed: ${error.message}")
                        publish(
                            parsed,
                            TunnelStatus(
                                state = TunnelState.ERROR,
                                error = error.message ?: "tunnel failed",
                            ),
                        )
                    }
                }
                return START_NOT_STICKY
            }
        }
        return START_NOT_STICKY
    }

    private fun publish(profile: TunnelProfile, status: TunnelStatus) {
        lastStatus = status
        if (status.state == TunnelState.ERROR) {
            log("error: ${status.error}")
        } else if (status.state == TunnelState.UP) {
            log(
                "up ${status.endpoint} rx=${status.rxBytes} tx=${status.txBytes} " +
                    "handshake=${status.lastHandshake}",
            )
        }
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(NOTIFICATION_ID, buildNotification(profile, statusText(status)))
        MainActivity.nativeTunnelEvent(status.toJson())
    }

    private fun startHandshakeWatchdog(profile: TunnelProfile) {
        watchdogJob?.cancel()
        watchdogJob = scope.launch {
            repeat(15) {
                delay(2_000)
                val status = TunnelController.status() ?: return@launch
                if (status.state != TunnelState.UP) return@launch
                if (status.lastHandshake > 0 || status.rxBytes > 0) return@launch
            }
            log("handshake timeout")
            publish(
                profile,
                TunnelStatus(
                    state = TunnelState.ERROR,
                    error = "WireGuard handshake timed out. Check the endpoint, UDP port, and firewall.",
                ),
            )
            TunnelController.disconnect()
        }
    }

    private fun statusText(status: TunnelStatus): String {
        if (status.state == TunnelState.ERROR) {
            return status.error ?: "Connection failed"
        }
        if (status.state != TunnelState.UP) {
            return when (status.state) {
                TunnelState.CONNECTING -> "Connecting…"
                TunnelState.DISCONNECTING -> "Disconnecting…"
                else -> "Disconnected"
            }
        }
        return "↓ ${formatBytes(status.rxBytes)}   ↑ ${formatBytes(status.txBytes)}"
    }

    private fun buildNotification(profile: TunnelProfile, text: String): Notification {
        val intent = Intent(this, MainActivity::class.java)
        val pending = PendingIntent.getActivity(
            this,
            0,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val accent = accentColor(profile.accent)
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_moon)
            .setColor(accent)
            .setContentTitle(profile.name)
            .setContentText(text)
            .setContentIntent(pending)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
    }

    private fun startForegroundCompat(notification: Notification) {
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

    private fun stopForegroundCompat() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
    }

    override fun onDestroy() {
        watchdogJob?.cancel()
        scope.cancel()
        super.onDestroy()
    }

    private fun accentColor(name: String): Int = when (name.lowercase()) {
        "blue" -> Color.parseColor("#60A5FA")
        "red" -> Color.parseColor("#F87171")
        "green" -> Color.parseColor("#4ADE80")
        "purple" -> Color.parseColor("#A78BFA")
        "orange" -> Color.parseColor("#FB923C")
        "pink" -> Color.parseColor("#F472B6")
        "gray" -> Color.parseColor("#94A3B8")
        else -> Color.parseColor("#06B6D4")
    }

    private fun formatBytes(bytes: Long): String {
        if (bytes < 1024) return "$bytes B"
        val units = arrayOf("KB", "MB", "GB", "TB")
        var value = bytes.toDouble()
        var unit = -1
        while (value >= 1024 && unit < units.lastIndex) {
            value /= 1024
            unit++
        }
        return String.format("%.1f %s", value, units[unit])
    }
}
