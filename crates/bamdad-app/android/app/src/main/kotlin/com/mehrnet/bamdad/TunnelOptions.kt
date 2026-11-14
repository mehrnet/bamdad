package com.mehrnet.bamdad

import android.net.VpnService
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject

/**
 * Global tunnel options applied to every VpnService builder: custom DNS and
 * Android per-app split tunneling. Kept in a small object so both the proxy
 * VpnService and the OpenVPN/WireGuard backends can read it without threading
 * the settings through every JNI call.
 */
object TunnelOptions {
    private const val TAG = "BamdadTunnelOptions"

    @Volatile
    var dns: List<String> = emptyList()
        private set

    @Volatile
    var splitMode: String = "off"
        private set

    @Volatile
    var splitApps: List<String> = emptyList()
        private set

    fun configure(payload: String) {
        val json = runCatching { JSONObject(payload) }.getOrNull() ?: return
        dns = json.optJSONArray("dns").toStringList()
        splitMode = json.optString("split_mode", "off")
        splitApps = json.optJSONArray("split_apps").toStringList()
    }

    fun apply(builder: VpnService.Builder) {
        dns.forEach { address ->
            runCatching { builder.addDnsServer(address) }
                .onFailure { Log.w(TAG, "bad DNS $address: ${it.message}") }
        }
        when (splitMode) {
            "allow" -> splitApps.forEach { pkg ->
                runCatching { builder.addAllowedApplication(pkg) }
                    .onFailure { Log.w(TAG, "allow $pkg failed: ${it.message}") }
            }
            "exclude" -> splitApps.forEach { pkg ->
                runCatching { builder.addDisallowedApplication(pkg) }
                    .onFailure { Log.w(TAG, "exclude $pkg failed: ${it.message}") }
            }
            else -> Unit
        }
    }

    private fun JSONArray?.toStringList(): List<String> {
        if (this == null) return emptyList()
        return buildList {
            for (index in 0 until length()) {
                val value = optString(index).trim()
                if (value.isNotEmpty()) add(value)
            }
        }
    }
}
