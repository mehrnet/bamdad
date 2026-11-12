package com.mehrnet.bamdad

import android.content.Context
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.amnezia.awg.backend.GoBackend
import org.amnezia.awg.backend.Tunnel
import org.amnezia.awg.config.Config
import java.io.BufferedReader
import java.io.StringReader
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress

/**
 * WireGuard and AmneziaWG through the userspace Go backend. The backend owns
 * its own Android VpnService; no SOCKS or tun2socks is involved.
 */
class AmneziaWgBackend(context: Context) : TunnelBackend {
    companion object {
        private const val TAG = "BamdadAmnezia"
        private const val TUNNEL_NAME = "bamdad0"
    }

    override val transport: TunnelTransport = TunnelTransport.AMNEZIAWG
    override var onStatus: ((TunnelStatus) -> Unit)? = null

    private val backend = GoBackend(context)
    private var config: Config? = null
    @Volatile
    private var state: Tunnel.State = Tunnel.State.DOWN
    @Volatile
    private var connectedSince: Long = 0L

    private val tunnel = object : Tunnel {
        override fun getName(): String = TUNNEL_NAME

        override fun onStateChange(newState: Tunnel.State) {
            Log.i(TAG, "tunnel state: ${state} -> ${newState}")
            state = newState
            connectedSince = if (newState == Tunnel.State.UP) {
                System.currentTimeMillis()
            } else {
                0L
            }
            publish()
        }
    }

    override suspend fun connect(profile: TunnelProfile) {
        withContext(Dispatchers.IO) {
            state = Tunnel.State.DOWN
            publish()
            val clean = profile.config.trimStart { it.isWhitespace() || it == '\uFEFF' || it == '\u200B' }
            val resolved = resolveEndpoint(clean)
            val parsed = Config.parse(BufferedReader(StringReader(resolved)))
            config = parsed
            val endpoint = parsed.peers.firstOrNull()?.endpoint?.orElse(null)
            val address = parsed.`interface`.addresses.joinToString(", ")
            MainActivity.nativeLog(
                "BamdadAmnezia: parsed ${profile.name} endpoint=$endpoint address=$address",
            )
            backend.setState(tunnel, Tunnel.State.UP, parsed)
        }
    }

    override suspend fun disconnect() {
        withContext(Dispatchers.IO) {
            try {
                backend.setState(tunnel, Tunnel.State.DOWN, null)
            } catch (error: Throwable) {
                Log.w(TAG, "disconnect failed: ${error.message}")
            }
            config = null
            state = Tunnel.State.DOWN
            connectedSince = 0L
            publish()
        }
    }

    override fun status(): TunnelStatus {
        val active = config
        val stats = if (state == Tunnel.State.UP && active != null) {
            runCatching { backend.getStatistics(tunnel) }.getOrNull()
        } else {
            null
        }
        var rx = 0L
        var tx = 0L
        var handshake = 0L
        if (stats != null && active != null) {
            for (peer in active.peers) {
                val peerStats = stats.peer(peer.publicKey)
                if (peerStats != null) {
                    rx += peerStats.rxBytes
                    tx += peerStats.txBytes
                    handshake = maxOf(handshake, peerStats.latestHandshakeEpochMillis)
                }
            }
        }
        val endpoint = active?.peers?.firstOrNull()?.endpoint?.orElse(null)?.toString().orEmpty()
        val address = active?.`interface`?.addresses?.joinToString(", ") { it.toString() }.orEmpty()
        return TunnelStatus(
            state = when (state) {
                Tunnel.State.UP -> TunnelState.UP
                else -> TunnelState.DOWN
            },
            rxBytes = rx,
            txBytes = tx,
            endpoint = endpoint,
            localAddress = address,
            lastHandshake = handshake,
        )
    }

    /**
     * Resolve a hostname endpoint on the physical network before the tunnel
     * starts. This avoids a race where the proxy VpnService is still being
     * torn down and the Go backend's DNS lookup has no usable resolver.
     */
    private fun resolveEndpoint(config: String): String {
        val lines = config.lines().toMutableList()
        for ((index, line) in lines.withIndex()) {
            val trimmed = line.trim()
            if (!trimmed.startsWith("Endpoint", ignoreCase = true) || !trimmed.contains('=')) {
                continue
            }
            val value = trimmed.substringAfter('=').trim()
            val (host, port) = splitHostPort(value) ?: continue
            if (android.net.InetAddresses.isNumericAddress(host)) {
                continue
            }
            val addresses = InetAddress.getAllByName(host)
            val address = addresses.firstOrNull { it is Inet4Address } ?: addresses.first()
            val formatted = if (address is Inet6Address) "[${address.hostAddress}]" else address.hostAddress
            lines[index] = line.substringBefore('=') + "= $formatted:$port"
            MainActivity.nativeLog("BamdadAmnezia: resolved $host -> $formatted:$port")
        }
        return lines.joinToString("\n")
    }

    private fun splitHostPort(value: String): Pair<String, Int>? {
        if (value.startsWith("[")) {
            val end = value.indexOf(']')
            if (end <= 1) return null
            val port = value.substring(end + 1).removePrefix(":").toIntOrNull() ?: return null
            return value.substring(1, end) to port
        }
        val separator = value.lastIndexOf(':')
        if (separator <= 0) return null
        val port = value.substring(separator + 1).toIntOrNull() ?: return null
        return value.substring(0, separator) to port
    }

    private fun publish() {
        onStatus?.invoke(status())
    }
}
