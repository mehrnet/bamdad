package com.mehrnet.bamdad

import android.util.Log
import java.math.BigInteger
import java.net.InetAddress

/** One IPv4 or IPv6 prefix, e.g. `10.0.0.0/8`. */
data class Cidr(val address: String, val prefix: Int, val ipv6: Boolean) {
    override fun toString(): String = "$address/$prefix"
}

/**
 * Prefix arithmetic for bypass routes. Android has no "exclude this subnet
 * from the tunnel" switch below API 33, so a bypassed range is removed by
 * subtracting it from the routed prefix and sending the remaining prefixes to
 * the VpnService or WireGuard `AllowedIPs`.
 */
object CidrRoutes {
    private const val TAG = "BamdadCidr"

    fun parse(text: String): Cidr? {
        val value = text.trim()
        if (value.isEmpty()) return null
        val slash = value.indexOf('/')
        val host = if (slash >= 0) value.substring(0, slash) else value
        val ipv6 = host.contains(':')
        val bits = if (ipv6) 128 else 32
        val prefix = if (slash >= 0) {
            value.substring(slash + 1).trim().toIntOrNull() ?: return null
        } else {
            bits
        }
        if (prefix !in 0..bits) return null
        val address = numericAddress(host, ipv6) ?: return null
        return Cidr(address.hostAddress ?: return null, prefix, ipv6)
    }

    fun parseAll(values: List<String>): List<Cidr> = values.mapNotNull { value ->
        parse(value) ?: run {
            Log.w(TAG, "ignoring invalid CIDR $value")
            null
        }
    }

    /** [base] minus every [excludes] prefix that overlaps it. */
    fun subtract(base: Cidr, excludes: List<Cidr>): List<Cidr> {
        val bits = if (base.ipv6) 128 else 32
        val (start, end) = range(base, bits) ?: return listOf(base)
        val inside = excludes.asSequence()
            .filter { it.ipv6 == base.ipv6 }
            .mapNotNull { range(it, bits) }
            .filter { it.second >= start && it.first <= end }
            .sortedBy { it.first }
            .toList()
        if (inside.isEmpty()) return listOf(base)

        val out = mutableListOf<Cidr>()
        var cursor = start
        for ((blockStart, blockEnd) in inside) {
            if (blockEnd < cursor) continue
            if (blockStart > cursor) {
                out += prefixes(cursor, blockStart - BigInteger.ONE, bits)
            }
            cursor = maxOf(cursor, blockEnd + BigInteger.ONE)
            if (cursor > end) break
        }
        if (cursor <= end) out += prefixes(cursor, end, bits)
        return out
    }

    /** [base] minus [bypass], keeping the base prefix when nothing is left. */
    fun subtractKeeping(base: Cidr, bypass: List<Cidr>): List<Cidr> =
        subtract(base, bypass).ifEmpty {
            Log.w(TAG, "bypass list covers all of $base; keeping the route")
            listOf(base)
        }

    /** Decompose the inclusive integer range into aligned prefixes. */
    private fun prefixes(start: BigInteger, end: BigInteger, bits: Int): List<Cidr> {
        val out = mutableListOf<Cidr>()
        var current = start
        while (current <= end) {
            var size = BigInteger.ONE
            var prefix = bits
            while (prefix > 0) {
                val next = size.shiftLeft(1)
                if (current.mod(next) != BigInteger.ZERO) break
                if (current.add(next) - BigInteger.ONE > end) break
                size = next
                prefix--
            }
            out += Cidr(format(current, bits), prefix, bits == 128)
            current += size
        }
        return out
    }

    private fun range(cidr: Cidr, bits: Int): Pair<BigInteger, BigInteger>? {
        val address = numericAddress(cidr.address, cidr.ipv6) ?: return null
        val value = BigInteger(1, address.address)
        val hostBits = bits - cidr.prefix
        val size = BigInteger.ONE.shiftLeft(hostBits)
        val start = value - value.mod(size)
        return start to (start + size - BigInteger.ONE)
    }

    private fun format(value: BigInteger, bits: Int): String {
        val length = bits / 8
        val raw = value.toByteArray()
        val bytes = ByteArray(length)
        val meaningful = if (raw.size > length) raw.copyOfRange(raw.size - length, raw.size) else raw
        meaningful.copyInto(bytes, length - meaningful.size)
        return InetAddress.getByAddress(bytes).hostAddress ?: "0.0.0.0"
    }

    /** Only literal addresses are accepted; hostnames must never trigger DNS here. */
    private fun numericAddress(host: String, ipv6: Boolean): InetAddress? {
        val allowed = if (ipv6) "0123456789abcdefABCDEF:." else "0123456789."
        if (host.isEmpty() || host.any { it !in allowed }) return null
        return runCatching { InetAddress.getByName(host) }.getOrNull()
    }
}
