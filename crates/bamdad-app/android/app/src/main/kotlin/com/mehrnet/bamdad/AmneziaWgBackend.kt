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
            val parsed = Config.parse(BufferedReader(StringReader(clean)))
            config = parsed
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
        if (stats != null && active != null) {
            for (peer in active.peers) {
                val peerStats = stats.peer(peer.publicKey)
                if (peerStats != null) {
                    rx += peerStats.rxBytes
                    tx += peerStats.txBytes
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
        )
    }

    private fun publish() {
        onStatus?.invoke(status())
    }
}
