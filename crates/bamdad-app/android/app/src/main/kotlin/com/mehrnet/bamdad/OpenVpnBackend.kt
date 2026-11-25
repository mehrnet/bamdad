package com.mehrnet.bamdad

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/**
 * OpenVPN transport. The OpenVPN 3 core runs in the `bamdad_openvpn3` native
 * library; [OpenVpnService] owns the tun device and reports events here.
 */
class OpenVpnBackend(private val context: Context) : TunnelBackend {
    companion object {
        private const val TAG = "BamdadOpenVpn"
        private const val CONNECT_TIMEOUT_MS = 25_000L
    }

    override val transport: TunnelTransport = TunnelTransport.OPENVPN
    override var onStatus: ((TunnelStatus) -> Unit)? = null

    override suspend fun connect(profile: TunnelProfile) {
        withContext(Dispatchers.IO) {
            MainActivity.nativeLog("BamdadOpenVpn: starting ${profile.name}")
            publish(TunnelStatus(TunnelState.CONNECTING, endpoint = OpenVpnService.snapshot().endpoint))
            OpenVpnService.start(context, profile, profile.authUser, profile.authPass)
            val deadline = System.currentTimeMillis() + CONNECT_TIMEOUT_MS
            while (true) {
                val state = OpenVpnService.snapshot().state
                if (state == TunnelState.UP || state == TunnelState.ERROR) break
                if (System.currentTimeMillis() >= deadline) {
                    // The core never settled; stop it instead of leaving an
                    // endless reconnect loop in the background.
                    OpenVpnService.stop(context)
                    throw IllegalStateException("OpenVPN could not connect")
                }
                delay(200)
            }
            val status = status()
            publish(status)
            if (status.state == TunnelState.ERROR) {
                OpenVpnService.stop(context)
                throw IllegalStateException(status.error ?: "OpenVPN connection failed")
            }
        }
    }

    override suspend fun disconnect() {
        withContext(Dispatchers.IO) {
            // No status event here: Rust owns the bar during a stop/switch,
            // and a late DOWN would close the next attempt's event gate.
            OpenVpnService.stop(context)
        }
    }

    override fun released(): Boolean? = OpenVpnService.released()

    override fun status(): TunnelStatus {
        val snapshot = OpenVpnService.snapshot()
        val (rx, tx) = OpenVpnService.statistics()
        return snapshot.copy(rxBytes = rx, txBytes = tx)
    }

    private fun publish(status: TunnelStatus) {
        onStatus?.invoke(status)
    }
}
