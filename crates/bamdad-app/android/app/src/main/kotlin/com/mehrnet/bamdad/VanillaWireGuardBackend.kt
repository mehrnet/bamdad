package com.mehrnet.bamdad

import android.content.Context
import android.util.Log
import com.wireguard.android.backend.GoBackend
import com.wireguard.android.backend.Tunnel
import com.wireguard.config.Config
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.BufferedReader
import java.io.StringReader

/**
 * Vanilla WireGuard through the official WireGuard Android userspace backend.
 * AmneziaWG remains on the amneziawg-android backend.
 */
class VanillaWireGuardBackend(context: Context) : TunnelBackend {
    companion object {
        private const val TAG = "BamdadWireGuard"
        private const val TUNNEL_NAME = "bamdad1"
    }

    override val transport: TunnelTransport = TunnelTransport.WIREGUARD
    override var onStatus: ((TunnelStatus) -> Unit)? = null

    private val backend = GoBackend(context)
    private var config: Config? = null
    @Volatile
    private var state: Tunnel.State = Tunnel.State.DOWN

    private val tunnel = object : Tunnel {
        override fun getName(): String = TUNNEL_NAME

        override fun onStateChange(newState: Tunnel.State) {
            Log.i(TAG, "tunnel state: ${state} -> $newState")
            state = newState
            publish()
        }
    }

    override suspend fun connect(profile: TunnelProfile) {
        withContext(Dispatchers.IO) {
            state = Tunnel.State.DOWN
            publish()
            val clean = profile.config.trimStart { it.isWhitespace() || it == '\uFEFF' || it == '\u200B' }
            val resolved = WireGuardConfig.prepare(clean)
            val parsed = Config.parse(BufferedReader(StringReader(resolved)))
            config = parsed
            val endpoint = parsed.peers.firstOrNull()?.endpoint?.orElse(null)
            val address = parsed.`interface`.addresses.joinToString(", ")
            MainActivity.nativeLog("BamdadWireGuard: parsed ${profile.name} endpoint=$endpoint address=$address")
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
                val peerStats = stats.peer(peer.publicKey) ?: continue
                rx += peerStats.rxBytes
                tx += peerStats.txBytes
                handshake = maxOf(handshake, peerStats.latestHandshakeEpochMillis)
            }
        }
        val endpoint = active?.peers?.firstOrNull()?.endpoint?.orElse(null)?.toString().orEmpty()
        val address = active?.`interface`?.addresses?.joinToString(", ") { it.toString() }.orEmpty()
        return TunnelStatus(
            state = if (state == Tunnel.State.UP) TunnelState.UP else TunnelState.DOWN,
            rxBytes = rx,
            txBytes = tx,
            endpoint = endpoint,
            localAddress = address,
            lastHandshake = handshake,
        )
    }

    private fun publish() {
        onStatus?.invoke(status())
    }
}
