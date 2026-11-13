package com.mehrnet.bamdad

import android.content.Context
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Serializes native tunnel backends. Android has one VPN slot, so a new
 * backend may only start after the previous one has released its service.
 */
object TunnelController {
    private val mutex = Mutex()
    private var active: TunnelBackend? = null

    suspend fun connect(
        context: Context,
        profile: TunnelProfile,
        onStatus: (TunnelStatus) -> Unit,
    ) = mutex.withLock {
        active?.let {
            runCatching { it.disconnect() }
            // Give the backend's VpnService time to release the single
            // Android VPN slot before the next backend establishes.
            delay(800)
        }
        active = null

        val backend = when (profile.transport) {
            TunnelTransport.WIREGUARD, TunnelTransport.AMNEZIAWG -> AmneziaWgBackend(context)
            TunnelTransport.OPENVPN -> OpenVpnBackend(context)
            else -> error("transport ${profile.transport} is not implemented yet")
        }
        backend.onStatus = onStatus
        active = backend
        backend.connect(profile)
    }

    suspend fun disconnect() = mutex.withLock {
        active?.let { runCatching { it.disconnect() } }
        active = null
    }

    fun status(): TunnelStatus? = active?.status()
}
