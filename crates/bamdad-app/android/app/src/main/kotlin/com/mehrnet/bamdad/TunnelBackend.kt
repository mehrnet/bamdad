package com.mehrnet.bamdad

import org.json.JSONArray
import org.json.JSONObject

enum class TunnelTransport {
    PROXY,
    WIREGUARD,
    AMNEZIAWG,
    IPSEC,
    OPENVPN;

    companion object {
        fun from(value: String): TunnelTransport = when (value.lowercase()) {
            "wireguard" -> WIREGUARD
            "amneziawg" -> AMNEZIAWG
            "ipsec" -> IPSEC
            "openvpn" -> OPENVPN
            else -> PROXY
        }
    }
}

enum class TunnelState {
    DOWN,
    CONNECTING,
    UP,
    DISCONNECTING,
    ERROR;

    companion object {
        fun from(value: String): TunnelState = when (value.lowercase()) {
            "connecting" -> CONNECTING
            "up" -> UP
            "disconnecting" -> DISCONNECTING
            "error" -> ERROR
            else -> DOWN
        }
    }
}

data class TunnelProfile(
    val id: String,
    val name: String,
    val transport: TunnelTransport,
    val config: String,
    val dns: List<String> = emptyList(),
    val allowedApps: List<String> = emptyList(),
    val excludedApps: List<String> = emptyList(),
    val bypassCidrs: List<String> = emptyList(),
    val mtu: Int? = null,
    val accent: String = "cyan",
) {
    companion object {
        fun fromJson(text: String): TunnelProfile {
            val json = JSONObject(text)
            return TunnelProfile(
                id = json.optString("id"),
                name = json.optString("name", "Bamdad"),
                transport = TunnelTransport.from(json.optString("transport")),
                config = json.optString("config"),
                dns = json.stringList("dns"),
                allowedApps = json.stringList("allowed_apps"),
                excludedApps = json.stringList("excluded_apps"),
                bypassCidrs = json.stringList("bypass_cidrs"),
                mtu = if (json.has("mtu")) json.optInt("mtu") else null,
                accent = json.optString("accent", "cyan"),
            )
        }
    }
}

private fun JSONObject.stringList(name: String): List<String> {
    val value = optJSONArray(name) ?: return emptyList()
    return buildList {
        for (index in 0 until value.length()) {
            add(value.optString(index))
        }
    }
}

data class TunnelStatus(
    val state: TunnelState,
    val rxBytes: Long = 0L,
    val txBytes: Long = 0L,
    val endpoint: String = "",
    val localAddress: String = "",
    val error: String? = null,
) {
    fun toJson(): String = JSONObject().apply {
        put("state", state.name.lowercase())
        put("rx_bytes", rxBytes)
        put("tx_bytes", txBytes)
        put("endpoint", endpoint)
        put("local_address", localAddress)
        put("last_handshake", lastHandshake)
        error?.let { put("error", it) }
    }.toString()
}

interface TunnelBackend {
    val transport: TunnelTransport
    var onStatus: ((TunnelStatus) -> Unit)?

    suspend fun connect(profile: TunnelProfile)
    suspend fun disconnect()
    fun status(): TunnelStatus
}
