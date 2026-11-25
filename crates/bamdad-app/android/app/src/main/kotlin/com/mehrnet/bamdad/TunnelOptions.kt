package com.mehrnet.bamdad

import android.net.VpnService
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject

/**
 * Global tunnel options applied to every VpnService builder: custom DNS,
 * Android per-app split tunneling, and bypass routes. Kept in a small object so
 * both the proxy VpnService and the OpenVPN/WireGuard backends can read it
 * without threading the settings through every JNI call.
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

    /** CIDR prefixes that must stay outside the tunnel. */
    @Volatile
    var bypass: List<Cidr> = emptyList()
        private set

    fun configure(payload: String) {
        val json = runCatching { JSONObject(payload) }.getOrNull() ?: return
        dns = json.optJSONArray("dns").toStringList()
        splitMode = json.optString("split_mode", "off")
        splitApps = json.optJSONArray("split_apps").toStringList()
        bypass = CidrRoutes.parseAll(json.optJSONArray("bypass").toStringList())
    }

    /** [base] with every bypassed range cut out of it. */
    fun bypassRoutes(base: String): List<Cidr> {
        val parsed = CidrRoutes.parse(base) ?: return emptyList()
        if (bypass.isEmpty()) return listOf(parsed)
        return CidrRoutes.subtractKeeping(parsed, bypass)
    }

    fun apply(builder: VpnService.Builder, selfPackage: String = "") {
        dns.forEach { address ->
            runCatching { builder.addDnsServer(address) }
                .onFailure { Log.w(TAG, "bad DNS $address: ${it.message}") }
        }
        when (splitMode) {
            "allow" -> splitApps.forEach { pkg ->
                // The core shares our UID: putting it on the allow list would
                // route its own sockets into its own tunnel.
                if (pkg == selfPackage) {
                    Log.w(TAG, "not adding $pkg to its own allow list")
                    return@forEach
                }
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
