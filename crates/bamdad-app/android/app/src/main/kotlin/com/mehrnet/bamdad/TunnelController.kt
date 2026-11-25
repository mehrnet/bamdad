package com.mehrnet.bamdad

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.util.concurrent.atomic.AtomicLong

/**
 * Owns the single Android VPN slot.
 *
 * Handover (tearing the previous backend down and waiting for the slot) is
 * serialized, but *waiting for a tunnel to come up* is not: a stop or a new
 * profile pick has to interrupt a connect that is stuck on an unreachable
 * server. Every attempt gets a ticket, and the running connect is a job that a
 * newer attempt or a disconnect cancels before touching the slot.
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

    private val handover = Mutex()
    @Volatile
    private var active: TunnelBackend? = null
    @Volatile
    private var connectJob: Job? = null

    /** Bumped by every connect and disconnect; older attempts stop early. */
    private val attempt = AtomicLong(0)

    /**
     * Connects [profile], cancelling whatever was running before. Returns
     * `true` when this attempt owns a live tunnel. A failure is reported to
     * [onStatus] as [TunnelState.ERROR] and returns `false`; an attempt that
     * was superseded returns `false` without touching the UI.
     */
    suspend fun connect(
        context: Context,
        profile: TunnelProfile,
        onStatus: (TunnelStatus) -> Unit,
    ): Boolean {
        val mine = attempt.incrementAndGet()
        cancelRunningConnect()
        disconnectActive()
        if (attempt.get() != mine) return false

        val backend = backendFor(context, profile)
        backend.onStatus = onStatus
        val owned = handover.withLock {
            if (attempt.get() != mine) {
                false
            } else {
                active = backend
                connectJob = currentCoroutineContext()[Job]
                true
            }
        }
        if (!owned) return false

        var connected = false
        try {
            backend.connect(profile)
            connected = attempt.get() == mine
            if (!connected) {
                Log.i(TAG, "connect for ${profile.name} was superseded")
            }
            return connected
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Throwable) {
            Log.w(TAG, "connect failed: ${error.message}", error)
            if (attempt.get() == mine) {
                onStatus(
                    TunnelStatus(
                        state = TunnelState.ERROR,
                        error = error.message ?: "Tunnel failed",
                    ),
                )
            }
            return false
        } finally {
            withContext(NonCancellable) {
                val mineJob = currentCoroutineContext()[Job]
                handover.withLock {
                    if (connectJob === mineJob) connectJob = null
                }
                if (!connected) releaseIfOwner(backend)
            }
        }
    }

    /** Stops the tunnel immediately, interrupting a connect that is in flight. */
    suspend fun disconnect() {
        attempt.incrementAndGet()
        cancelRunningConnect()
        disconnectActive()
    }

    fun status(): TunnelStatus? = active?.status()

    /** Whether the active backend has released the VPN slot (see [TunnelBackend.released]). */
    fun released(): Boolean? = active?.released()

    /** The backend owning the VPN slot right now, if any. */
    fun current(): TunnelBackend? = active

    private fun backendFor(context: Context, profile: TunnelProfile): TunnelBackend =
        when (profile.transport) {
            TunnelTransport.WIREGUARD, TunnelTransport.AMNEZIAWG -> AmneziaWgBackend(context)
            TunnelTransport.OPENVPN -> OpenVpnBackend(context)
            TunnelTransport.IPSEC -> IpSecBackend(context)
            else -> error("transport ${profile.transport} is not implemented yet")
        }

    /**
     * Cancels the coroutine that is waiting for a tunnel. Its `finally` tears
     * the backend down when it still owns the slot, so this returns with the
     * slot free (or with the next owner already in place).
     */
    private suspend fun cancelRunningConnect() {
        val job = connectJob ?: return
        if (job === currentCoroutineContext()[Job]) return
        // `join` also covers a connect that has just finished but has not
        // cleared `connectJob` yet.
        job.cancel()
        job.join()
    }

    /**
     * Disconnects the current backend and waits for it to release the slot.
     * The handover lock is held for the whole teardown so a connect cannot
     * start a new service while the old one is still shutting down (stopping
     * the same service class would kill the new tunnel).
     */
    private suspend fun disconnectActive() {
        handover.withLock {
            val previous = active ?: return@withLock
            active = null
            runCatching { previous.disconnect() }
                .onFailure { Log.w(TAG, "disconnect failed: ${it.message}") }
            awaitRelease(previous)
        }
    }

    /**
     * Tears the backend down only if it still owns the slot. A newer attempt
     * clears [active] before stopping the old backend, so this can never kill
     * the tunnel that replaced it.
     */
    private suspend fun releaseIfOwner(backend: TunnelBackend) {
        withContext(NonCancellable) {
            handover.withLock {
                if (active !== backend) return@withLock
                active = null
                runCatching { backend.disconnect() }
                // The next owner may start as soon as this coroutine joins;
                // do not hand it a slot that the old service still holds.
                awaitRelease(backend)
            }
        }
    }

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
