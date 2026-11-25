package com.mehrnet.bamdad

import android.content.Context
import android.util.Log
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Serializes native tunnel backends. Android has one VPN slot, so a new
 * backend may only start after the previous one has released its service.
 */
object TunnelController {
    private const val TAG = "BamdadTunnel"

    /** Upper bound on how long we wait for the old service to go away. */
    private const val RELEASE_TIMEOUT_MS = 3_000L
    private const val RELEASE_POLL_MS = 50L

    /** Small grace after the service is gone, so the slot is really free. */
    private const val RELEASE_SETTLE_MS = 120L

    /** Settle time for backends that cannot report their service state. */
    private const val UNKNOWN_RELEASE_SETTLE_MS = 800L

    private val mutex = Mutex()
    @Volatile
    private var active: TunnelBackend? = null

    suspend fun connect(
        context: Context,
        profile: TunnelProfile,
        onStatus: (TunnelStatus) -> Unit,
    ) = mutex.withLock {
        active?.let { previous ->
            runCatching { previous.disconnect() }
                .onFailure { Log.w(TAG, "disconnect failed: ${it.message}") }
            awaitRelease(previous)
        }
        active = null

        val backend = when (profile.transport) {
            TunnelTransport.WIREGUARD, TunnelTransport.AMNEZIAWG -> AmneziaWgBackend(context)
            TunnelTransport.OPENVPN -> OpenVpnBackend(context)
            TunnelTransport.IPSEC -> IpSecBackend(context)
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

    /**
     * Waits until the previous backend's VpnService has torn down. A fixed
     * sleep was either too short (a slow `onRevoke` makes the next
     * `establish()` fail) or needlessly slow; poll the backend's real signal
     * instead, and keep the old settle time only when it cannot report one.
     */
    private suspend fun awaitRelease(previous: TunnelBackend) {
        val deadline = System.currentTimeMillis() + RELEASE_TIMEOUT_MS
        while (true) {
            val released = runCatching { previous.released() }.getOrNull()
            when (released) {
                null -> {
                    // e.g. the Go WireGuard backend hides its service lifecycle.
                    delay(UNKNOWN_RELEASE_SETTLE_MS)
                    return
                }
                true -> {
                    delay(RELEASE_SETTLE_MS)
                    return
                }
                false -> Unit
            }
            if (System.currentTimeMillis() >= deadline) {
                Log.w(TAG, "previous backend still holds the VPN slot after ${RELEASE_TIMEOUT_MS}ms")
                return
            }
            delay(RELEASE_POLL_MS)
        }
    }
}
