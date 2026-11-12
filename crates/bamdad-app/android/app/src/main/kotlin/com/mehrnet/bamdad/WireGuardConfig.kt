package com.mehrnet.bamdad

import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress

/**
 * WireGuard config normalization shared by the official and AmneziaWG
 * backends:
 *  - resolve a hostname endpoint on the physical network
 *  - remove the endpoint IP from AllowedIPs so the handshake can never be
 *    routed into its own tunnel during startup
 */
object WireGuardConfig {
    fun prepare(raw: String): String {
        val lines = raw.lines().toMutableList()
        var excludedIpv4: Int? = null
        for ((index, line) in lines.withIndex()) {
            val trimmed = line.trim()
            if (!trimmed.startsWith("Endpoint", ignoreCase = true) || !trimmed.contains('=')) {
                continue
            }
            val value = trimmed.substringAfter('=').trim()
            val (host, port) = splitHostPort(value) ?: continue
            val address = if (android.net.InetAddresses.isNumericAddress(host)) {
                InetAddress.getByName(host)
            } else {
                val addresses = InetAddress.getAllByName(host)
                addresses.firstOrNull { it is Inet4Address } ?: addresses.first()
            }
            val formatted = if (address is Inet6Address) "[${address.hostAddress}]" else address.hostAddress
            if (address is Inet4Address) {
                excludedIpv4 = ipv4ToInt(address.hostAddress)
            }
            lines[index] = line.substringBefore('=') + "= $formatted:$port"
            MainActivity.nativeLog("BamdadAmnezia: resolved $host -> $formatted:$port")
        }

        val excluded = excludedIpv4
        if (excluded != null) {
            for ((index, line) in lines.withIndex()) {
                val trimmed = line.trim()
                if (!trimmed.startsWith("AllowedIPs", ignoreCase = true) || !trimmed.contains('=')) {
                    continue
                }
                val value = trimmed.substringAfter('=').trim()
                val rewritten = value.split(',')
                    .map { it.trim() }
                    .filter { it.isNotEmpty() }
                    .flatMap { cidr -> excludeIpv4FromCidr(cidr, excluded) }
                if (rewritten.isNotEmpty()) {
                    lines[index] = line.substringBefore('=') + "= " + rewritten.joinToString(", ")
                    MainActivity.nativeLog(
                        "BamdadAmnezia: excluded endpoint from ${value.split(',').size} allowed CIDRs",
                    )
                }
            }
        }
        return lines.joinToString("\n")
    }

    private fun splitHostPort(value: String): Pair<String, Int>? {
        if (value.startsWith("[")) {
            val end = value.indexOf(']')
            if (end <= 1) return null
            val port = value.substring(end + 1).removePrefix(":").toIntOrNull() ?: return null
            return value.substring(1, end) to port
        }
        val separator = value.lastIndexOf(':')
        if (separator <= 0) return null
        val port = value.substring(separator + 1).toIntOrNull() ?: return null
        return value.substring(0, separator) to port
    }

    private fun ipv4ToInt(address: String): Int? {
        val parts = address.split('.')
        if (parts.size != 4) return null
        var value = 0
        for (part in parts) {
            val byte = part.toIntOrNull() ?: return null
            if (byte !in 0..255) return null
            value = (value shl 8) or byte
        }
        return value
    }

    private fun intToIpv4(value: Int): String =
        "${(value ushr 24) and 0xff}.${(value ushr 16) and 0xff}." +
            "${(value ushr 8) and 0xff}.${value and 0xff}"

    /** Return CIDRs covering [cidr] minus the single IPv4 [excluded] address. */
    private fun excludeIpv4FromCidr(cidr: String, excluded: Int): List<String> {
        val parts = cidr.split('/')
        if (parts.size != 2) return listOf(cidr)
        val address = ipv4ToInt(parts[0]) ?: return listOf(cidr)
        val prefix = parts[1].toIntOrNull() ?: return listOf(cidr)
        if (prefix !in 0..32) return listOf(cidr)
        val mask = if (prefix == 0) 0 else -1 shl (32 - prefix)
        if ((excluded and mask) != (address and mask)) return listOf(cidr)
        val result = mutableListOf<String>()
        var current = address
        var currentPrefix = prefix
        while (currentPrefix < 32) {
            val bit = 31 - currentPrefix
            val sibling = current xor (1 shl bit)
            result.add("${intToIpv4(sibling)}/${currentPrefix + 1}")
            currentPrefix++
        }
        return result
    }
}
