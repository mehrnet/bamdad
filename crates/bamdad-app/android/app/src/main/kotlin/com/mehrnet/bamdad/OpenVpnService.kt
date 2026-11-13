package com.mehrnet.bamdad

import android.content.Context
import android.content.Intent
import android.net.IpPrefix
import android.net.VpnService
import android.os.Build
import android.util.Log
import androidx.annotation.Keep
import org.json.JSONArray
import org.json.JSONObject
import java.net.InetAddress

/**
 * VpnService that owns the tun device for the OpenVPN 3 core.
 *
 * The native client builds the tun configuration (addresses, routes, DNS) and
 * calls [establishTun]; the returned descriptor is used directly by the core,
 * so no tun2socks is involved.
 */
class OpenVpnService : VpnService() {
    companion object {
        private const val TAG = "BamdadOpenVpn"

        const val ACTION_START = "com.mehrnet.bamdad.openvpn.START"
        const val ACTION_STOP = "com.mehrnet.bamdad.openvpn.STOP"
        const val EXTRA_CONFIG = "config"
        const val EXTRA_USERNAME = "username"
        const val EXTRA_PASSWORD = "password"

        @Volatile
        private var status = TunnelStatus(TunnelState.DOWN)

        fun snapshot(): TunnelStatus = status

        private fun update(newStatus: TunnelStatus) {
            status = newStatus
        }

        fun start(context: Context, profile: TunnelProfile, username: String = "", password: String = "") {
            update(TunnelStatus(TunnelState.CONNECTING, endpoint = remoteOf(profile.config)))
            val intent = Intent(context, OpenVpnService::class.java).apply {
                action = ACTION_START
                putExtra(EXTRA_CONFIG, profile.config)
                putExtra(EXTRA_USERNAME, username)
                putExtra(EXTRA_PASSWORD, password)
            }
            context.startService(intent)
        }

        fun stop(context: Context) {
            runCatching { OpenVpnNative.nativeStop() }
            runCatching {
                context.stopService(Intent(context, OpenVpnService::class.java))
            }
            update(TunnelStatus(TunnelState.DOWN, endpoint = status.endpoint))
        }

        fun statistics(): Pair<Long, Long> {
            val text = runCatching { OpenVpnNative.nativeStats() }.getOrNull() ?: return 0L to 0L
            return runCatching {
                val json = JSONObject(text)
                json.optLong("in", 0L) to json.optLong("out", 0L)
            }.getOrDefault(0L to 0L)
        }

        private fun remoteOf(config: String): String {
            for (line in config.lineSequence()) {
                val parts = line.trim().split(Regex("\\s+"))
                if (parts.size >= 2 && parts[0].equals("remote", ignoreCase = true)) {
                    return if (parts.size >= 3) "${parts[1]}:${parts[2]}" else parts[1]
                }
            }
            return ""
        }
    }

    private var established = false

    override fun onCreate() {
        super.onCreate()
        try {
            OpenVpnNative.nativeAttach(this)
        } catch (error: Throwable) {
            Log.e(TAG, "could not attach native service", error)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopSession()
                return START_NOT_STICKY
            }
            ACTION_START -> {
                val config = intent.getStringExtra(EXTRA_CONFIG)
                if (config.isNullOrBlank()) {
                    update(TunnelStatus(TunnelState.ERROR, error = "Empty OpenVPN profile"))
                    stopSelf()
                    return START_NOT_STICKY
                }
                val username = intent.getStringExtra(EXTRA_USERNAME).orEmpty()
                val password = intent.getStringExtra(EXTRA_PASSWORD).orEmpty()
                Thread({
                    try {
                        OpenVpnNative.nativeConnect(config, username, password, "")
                    } catch (error: Throwable) {
                        Log.e(TAG, "native connect failed", error)
                        update(
                            TunnelStatus(
                                TunnelState.ERROR,
                                endpoint = status.endpoint,
                                error = error.message ?: "OpenVPN failed",
                            ),
                        )
                    }
                }, "bamdad-openvpn").start()
                return START_NOT_STICKY
            }
        }
        return START_NOT_STICKY
    }

    private fun stopSession() {
        runCatching { OpenVpnNative.nativeStop() }
        releaseTun()
        update(TunnelStatus(TunnelState.DOWN, endpoint = status.endpoint))
        stopSelf()
    }

    private fun releaseTun() {
        // The descriptor was detached and handed to the core; it closes it on
        // teardown. Dropping the reference here is only bookkeeping.
        established = false
    }

    // -----------------------------------------------------------------------
    // Called from the native OpenVPN 3 worker
    // -----------------------------------------------------------------------

    @Keep
    fun establishTun(payload: String): Int {
        synchronized(this) {
            if (established) return -1
            val json = runCatching { JSONObject(payload) }.getOrNull() ?: return -1

            val builder = Builder()
                .setSession(json.optString("session").ifBlank { "Bamdad" })
                .setBlocking(false)

            val mtu = json.optInt("mtu", 0)
            if (mtu > 0) builder.setMtu(mtu)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                builder.setMetered(false)
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP_MR1) {
                builder.setUnderlyingNetworks(null)
            }

            var localAddress = ""
            val addresses = json.optJSONArray("addresses") ?: JSONArray()
            for (index in 0 until addresses.length()) {
                val entry = addresses.optJSONObject(index) ?: continue
                val address = entry.optString("addr")
                val ipv6 = entry.optBoolean("ipv6")
                val prefix = entry.optInt("prefix", if (ipv6) 128 else 32)
                try {
                    builder.addAddress(InetAddress.getByName(address), prefix)
                    if (localAddress.isEmpty()) localAddress = "$address/$prefix"
                } catch (error: Throwable) {
                    Log.w(TAG, "addAddress $address/$prefix failed: ${error.message}")
                }
            }

            val routes = json.optJSONArray("routes") ?: JSONArray()
            for (index in 0 until routes.length()) {
                val entry = routes.optJSONObject(index) ?: continue
                val address = entry.optString("addr")
                val ipv6 = entry.optBoolean("ipv6")
                val prefix = entry.optInt("prefix", if (ipv6) 128 else 32)
                try {
                    builder.addRoute(InetAddress.getByName(address), prefix)
                } catch (error: Throwable) {
                    Log.w(TAG, "addRoute $address/$prefix failed: ${error.message}")
                }
            }

            val excluded = json.optJSONArray("exclude_routes") ?: JSONArray()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                for (index in 0 until excluded.length()) {
                    val entry = excluded.optJSONObject(index) ?: continue
                    val address = entry.optString("addr")
                    val ipv6 = entry.optBoolean("ipv6")
                    val prefix = entry.optInt("prefix", if (ipv6) 128 else 32)
                    try {
                        builder.excludeRoute(IpPrefix(InetAddress.getByName(address), prefix))
                    } catch (error: Throwable) {
                        Log.w(TAG, "excludeRoute $address/$prefix failed: ${error.message}")
                    }
                }
            }

            val dns = json.optJSONArray("dns") ?: JSONArray()
            for (index in 0 until dns.length()) {
                val address = dns.optString(index)
                if (address.isBlank()) continue
                try {
                    builder.addDnsServer(InetAddress.getByName(address))
                } catch (error: Throwable) {
                    Log.w(TAG, "addDnsServer $address failed: ${error.message}")
                }
            }

            val fd = try {
                builder.establish()
            } catch (error: Throwable) {
                Log.e(TAG, "establish failed", error)
                null
            }
            if (fd == null) {
                update(
                    TunnelStatus(
                        TunnelState.ERROR,
                        endpoint = status.endpoint,
                        error = "Android rejected the VPN interface",
                    ),
                )
                return -1
            }

            established = true
            update(status.copy(localAddress = localAddress))
            Log.i(TAG, "tun established local=$localAddress mtu=$mtu")
            return fd.detachFd()
        }
    }

    @Keep
    fun teardown(disconnect: Boolean) {
        Log.i(TAG, "tun teardown (disconnect=$disconnect)")
        releaseTun()
        update(TunnelStatus(TunnelState.DOWN, endpoint = status.endpoint))
        stopSelf()
    }

    @Keep
    fun onEvent(payload: String) {
        val event = runCatching { JSONObject(payload) }.getOrNull() ?: return
        val name = event.optString("name")
        val info = event.optString("info")
        val fatal = event.optBoolean("fatal") || event.optBoolean("error")
        MainActivity.nativeLog("BamdadOpenVpn: event $name${if (info.isNotBlank()) " $info" else ""}")

        when (name) {
            "CONNECTED" -> update(TunnelStatus(TunnelState.UP, endpoint = status.endpoint, localAddress = status.localAddress))
            "DISCONNECTED" -> update(TunnelStatus(TunnelState.DOWN, endpoint = status.endpoint))
            "CONNECTING", "RESOLVE", "WAIT", "WAIT_PROXY", "AUTH", "AUTH_PENDING", "GET_CONFIG",
            "ASSIGN_IP", "ADD_ROUTES", "RECONNECTING", "RESUME", "RELAY",
            -> if (status.state != TunnelState.UP) {
                update(TunnelStatus(TunnelState.CONNECTING, endpoint = status.endpoint))
            }
            else -> if (fatal) {
                update(
                    TunnelStatus(
                        TunnelState.ERROR,
                        endpoint = status.endpoint,
                        error = if (info.isNotBlank()) info else name,
                    ),
                )
            }
        }
    }

    @Keep
    fun onLog(line: String) {
        Log.d(TAG, line)
        MainActivity.nativeLog("BamdadOpenVpn: $line")
    }

    override fun onDestroy() {
        runCatching { OpenVpnNative.nativeStop() }
        releaseTun()
        super.onDestroy()
    }
}
