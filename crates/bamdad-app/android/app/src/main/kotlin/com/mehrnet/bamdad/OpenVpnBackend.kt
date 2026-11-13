package com.mehrnet.bamdad

import android.content.Context
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * OpenVPN transport. The OpenVPN 3 core runs in the `bamdad_openvpn3` native
 * library; [OpenVpnService] owns the tun device and reports events here.
 */
class OpenVpnBackend(private val context: Context) : TunnelBackend {
    companion object {
        private const val TAG = "BamdadOpenVpn"
        private const val CONNECT_TIMEOUT_MS = 60_000L
    }

    override val transport: TunnelTransport = TunnelTransport.OPENVPN
    override var onStatus: ((TunnelStatus) -> Unit)? = null

    override suspend fun connect(profile: TunnelProfile) {
        withContext(Dispatchers.IO) {
            MainActivity.nativeLog("BamdadOpenVpn: starting ${profile.name}")
            OpenVpnService.start(context, profile, username = "", password = "")
            val settled = withTimeoutOrNull(CONNECT_TIMEOUT_MS) {
                while (true) {
                    val state = OpenVpnService.snapshot().state
                    if (state == TunnelState.UP || state == TunnelState.ERROR) break
                    delay(250)
                }
                true
            }
            if (settled == null) {
                Log.w(TAG, "connect timed out")
                OpenVpnService.stop(context)
                val status = TunnelStatus(TunnelState.ERROR, error = "OpenVPN connection timed out")
                publish(status)
                throw IllegalStateException(status.error)
            }
            val status = status()
            publish(status)
            if (status.state == TunnelState.ERROR) {
                throw IllegalStateException(status.error ?: "OpenVPN connection failed")
            }
        }
    }

    override suspend fun disconnect() {
        withContext(Dispatchers.IO) {
            OpenVpnService.stop(context)
            publish(TunnelStatus(TunnelState.DOWN))
        }
    }

    override fun status(): TunnelStatus {
        val snapshot = OpenVpnService.snapshot()
        val (rx, tx) = OpenVpnService.statistics()
        return snapshot.copy(rxBytes = rx, txBytes = tx)
    }

    private fun publish(status: TunnelStatus) {
        onStatus?.invoke(status)
    }
}
