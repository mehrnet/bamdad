package com.mehrnet.bamdad

/**
 * The subset of an `.sswan` (strongSwan Android profile) document that the
 * IpSec backend needs. Bamdad accepts the flat strongSwan XML and additionally
 * understands embedded `<ca>`, `<cert>` and `<key>` blocks so a profile can be
 * self-contained instead of pointing at strongSwan's own certificate database.
 */
data class IpSecConfig(
    val type: String,
    val server: String,
    val port: Int,
    val username: String?,
    val password: String?,
    val localId: String?,
    val remoteId: String?,
    val mtu: Int,
    val ikeProposal: String?,
    val espProposal: String?,
    val ca: String?,
    val cert: String?,
    val key: String?,
) {
    companion object {
        // No shared cache: parse() runs from the UI thread and from the connect
        // thread, and a plain HashMap cannot be mutated concurrently.
        private fun pattern(tag: String): Regex =
            Regex(
                "<$tag\\s*>(.*?)</$tag>",
                setOf(RegexOption.DOT_MATCHES_ALL, RegexOption.IGNORE_CASE),
            )

        private fun value(xml: String, tag: String): String? =
            pattern(tag).find(xml)?.groupValues?.get(1)?.trim()?.takeIf { it.isNotEmpty() }

        fun parse(xml: String): IpSecConfig {
            val type = value(xml, "type")
                ?.lowercase()
                ?.takeIf { it.startsWith("ikev2") }
                ?: "ikev2-eap"
            val port = value(xml, "port")?.toIntOrNull()?.takeIf { it in 1..65535 } ?: 500
            val mtu = value(xml, "mtu")?.toIntOrNull()?.takeIf { it in 600..1500 } ?: 1400
            return IpSecConfig(
                type = type,
                server = value(xml, "server").orEmpty(),
                port = port,
                username = value(xml, "username"),
                password = value(xml, "password"),
                localId = value(xml, "local_id") ?: value(xml, "local-id"),
                remoteId = value(xml, "remote_id") ?: value(xml, "remote-id") ?: value(xml, "server"),
                mtu = mtu,
                ikeProposal = value(xml, "ike_proposal") ?: value(xml, "ike-proposal"),
                espProposal = value(xml, "esp_proposal") ?: value(xml, "esp-proposal"),
                ca = value(xml, "ca"),
                cert = value(xml, "cert"),
                key = value(xml, "key"),
            )
        }
    }
}
