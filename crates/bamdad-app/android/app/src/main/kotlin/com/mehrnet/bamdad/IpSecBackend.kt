package com.mehrnet.bamdad

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.strongswan.android.logic.CharonVpnService

/**
 * IPSec/IKEv2 through the vendored strongSwan daemon. The VpnService slot is
 * owned by [CharonVpnService]; this backend just starts/stops it and mirrors
 * its state into the app's tunnel events.
 */
class IpSecBackend(private val context: Context) : TunnelBackend {
    override val transport: TunnelTransport = TunnelTransport.IPSEC
    override var onStatus: ((TunnelStatus) -> Unit)? = null

    private var endpoint = ""
    private var connectedSince = 0L

    override suspend fun connect(profile: TunnelProfile) {
        val config = IpSecConfig.parse(profile.config)
        endpoint = "${config.server}:${config.port}"
        withContext(Dispatchers.IO) {
            CharonVpnService.start(
                context,
                profile.name,
                profile.accent,
                profile.config,
                profile.authUser,
                profile.authPass,
            )
        }
        publish()
    }

    override suspend fun disconnect() {
        withContext(Dispatchers.IO) {
            CharonVpnService.stop(context)
        }
        connectedSince = 0L
        // No status event: Rust owns the bar during a stop/switch, and a late
        // DOWN would close the next attempt's event gate.
    }

    override fun released(): Boolean? = CharonVpnService.released()

    override fun status(): TunnelStatus {
        val state = CharonVpnService.state
        if (state == TunnelState.UP && connectedSince == 0L) {
            connectedSince = System.currentTimeMillis()
        }
        if (state != TunnelState.UP) {
            connectedSince = 0L
        }
        return TunnelStatus(
            state = state,
            error = CharonVpnService.error,
            endpoint = endpoint,
            localAddress = "",
            lastHandshake = connectedSince,
        )
    }

    private fun publish() {
        onStatus?.invoke(status())
    }
}
