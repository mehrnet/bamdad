package com.mehrnet.bamdad

import android.app.Service
import android.content.Intent
import android.os.Build
import android.util.Log
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Foreground owner for native tunnel backends. The backend itself owns the
 * Android VpnService; this service keeps the process alive and publishes the
 * transport-neutral status back to Rust.
 */
class TunnelService : Service() {
    companion object {
        private const val TAG = "BamdadTunnel"

        const val ACTION_START = "com.mehrnet.bamdad.tunnel.START"
        const val ACTION_STOP = "com.mehrnet.bamdad.tunnel.STOP"
        const val EXTRA_PROFILE = "profile"
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var profile: TunnelProfile? = null
    private var lastStatus: TunnelStatus? = null
    private var pollingJob: Job? = null
    private var lastLoggedState: TunnelState? = null

    /**
     * Bumped for every start command. A stop that was queued before a newer
     * start must not tear down that newer tunnel, cancel its polling, or
     * remove its notification: the two commands arrive milliseconds apart
     * when the user reconnects quickly.
     */
    @Volatile
    private var generation = 0

    private fun log(message: String) {
        Log.i(TAG, message)
        MainActivity.nativeLog("BamdadTunnel: $message")
    }

    override fun onBind(intent: Intent?): android.os.IBinder? = null

    override fun onCreate() {
        super.onCreate()
        BamdadNotification.ensureChannel(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                val mine = generation
                scope.launch {
                    if (generation == mine) {
                        pollingJob?.cancel()
                        TunnelController.disconnect()
                        // Rust already moved the bar to "Stopping…" and
                        // closes the event gate itself; publishing a DOWN
                        // here could land after the next attempt started.
                        stopForegroundCompat()
                    }
                    // Ignored when a newer start arrived meanwhile.
                    stopSelf(startId)
                }
                return START_NOT_STICKY
            }
            ACTION_START -> {
                generation++
                // The previous attempt's poller must not keep its handshake
                // watchdog armed against the tunnel we are about to start.
                pollingJob?.cancel()
                pollingJob = null
                val raw = intent.getStringExtra(EXTRA_PROFILE) ?: return START_NOT_STICKY
                val parsed = runCatching { TunnelProfile.fromJson(raw) }.getOrNull()
                    ?: run {
                        log("invalid native profile payload")
                        runCatching {
                            startForegroundCompat(
                                TunnelProfile("", "Bamdad", TunnelTransport.PROXY, ""),
                                "Invalid profile",
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
                    startForegroundCompat(parsed, "Connecting…")
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
                        val connected = TunnelController.connect(this@TunnelService, parsed) { status ->
                            publish(parsed, status)
                        }
                        if (connected) startStatusPolling(parsed)
                    } catch (cancelled: CancellationException) {
                        // A stop or a newer profile pick cancelled this
                        // attempt; it must not report an error.
                        throw cancelled
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
        if (status.state != lastLoggedState) {
            lastLoggedState = status.state
            when (status.state) {
                TunnelState.ERROR -> log("error: ${status.error}")
                TunnelState.UP -> log("up ${status.endpoint} ${status.localAddress}")
                else -> Unit
            }
        }
        BamdadNotification.update(
            this,
            BamdadNotification.TUNNEL_ID,
            profile.name,
            statusText(status),
            transportLabel(profile),
            profile.accent,
        )
        MainActivity.nativeTunnelEvent(status.toJson(profile.id))
    }

    private fun startStatusPolling(profile: TunnelProfile) {
        pollingJob?.cancel()
        pollingJob = scope.launch {
            var upPolls = 0
            while (isActive) {
                delay(1_000)
                val status = TunnelController.status() ?: break
                if (profile.transport == TunnelTransport.OPENVPN ||
                    profile.transport == TunnelTransport.IPSEC
                ) {
                    publish(profile, status)
                    if (status.state == TunnelState.DOWN || status.state == TunnelState.ERROR) break
                    continue
                }
                if (status.state != TunnelState.UP) {
                    publish(profile, status)
                    break
                }
                publish(profile, status)
                // The handshake watchdog is WireGuard-specific; OpenVPN
                // sessions have no handshake timestamp and reconnect on their
                // own.
                if (status.lastHandshake > 0) {
                    if (upPolls == 0) log("handshake established")
                    upPolls++
                    continue
                }
                upPolls++
                if (upPolls >= 30) {
                    log("handshake timeout")
                    publish(
                        profile,
                        TunnelStatus(
                            state = TunnelState.ERROR,
                            error = "WireGuard handshake timed out. Check the endpoint, UDP port, and firewall.",
                        ),
                    )
                    TunnelController.disconnect()
                    break
                }
            }
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

    /** Short transport label shown under the profile name. */
    private fun transportLabel(profile: TunnelProfile): String = when (profile.transport) {
        TunnelTransport.WIREGUARD -> "WireGuard"
        TunnelTransport.AMNEZIAWG -> "AmneziaWG"
        TunnelTransport.OPENVPN -> "OpenVPN"
        TunnelTransport.IPSEC -> "IPSec"
        TunnelTransport.PROXY -> "Proxy"
    }

    private fun startForegroundCompat(profile: TunnelProfile, text: String) {
        BamdadNotification.startForeground(
            this,
            BamdadNotification.TUNNEL_ID,
            profile.name,
            text,
            transportLabel(profile),
            profile.accent,
        )
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
        pollingJob?.cancel()
        scope.cancel()
        super.onDestroy()
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
