package org.strongswan.android.logic

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Base64
import android.util.Log
import com.mehrnet.bamdad.IpSecConfig
import com.mehrnet.bamdad.MainActivity
import com.mehrnet.bamdad.TunnelState
import java.io.ByteArrayInputStream
import java.io.File
import java.security.KeyFactory
import java.security.PrivateKey
import java.security.cert.CertificateFactory
import java.security.spec.PKCS8EncodedKeySpec

/**
 * Host for the vendored strongSwan charon daemon.
 *
 * The native bridge (`libandroidbridge.so`) resolves this exact class name and
 * the `BuilderAdapter` nested class, so it lives in strongSwan's package. Only
 * the surface the native code calls is implemented: the upstream UI, profile
 * database, and notification code are not used.
 *
 * The service owns the VpnService slot for IPSec/IKEv2. Charon builds the tun
 * device through [BuilderAdapter] once the first CHILD_SA comes up, and reports
 * progress through [updateStatus].
 */
class CharonVpnService : VpnService() {
    companion object {
        private const val TAG = "BamdadCharon"
        private const val ACTION_CONNECT = "com.mehrnet.bamdad.ipsec.CONNECT"
        private const val ACTION_STOP = "com.mehrnet.bamdad.ipsec.STOP"
        private const val EXTRA_NAME = "name"
        private const val EXTRA_ACCENT = "accent"
        private const val EXTRA_CONFIG = "config"
        private const val EXTRA_USER = "user"
        private const val EXTRA_PASS = "pass"

        // strongSwan state codes (see charonservice.h).
        private const val CHILD_SA_UP = 1
        private const val CHILD_SA_DOWN = 2
        private const val AUTH_ERROR = 3
        private const val PEER_AUTH_ERROR = 4
        private const val LOOKUP_ERROR = 5
        private const val UNREACHABLE_ERROR = 6
        private const val CERTIFICATE_UNAVAILABLE = 7
        private const val GENERIC_ERROR = 8

        init {
            System.loadLibrary("androidbridge")
        }

        @Volatile
        private var instance: CharonVpnService? = null

        /** Latest connection state, polled by [com.mehrnet.bamdad.IpSecBackend]. */
        @Volatile
        var state: TunnelState = TunnelState.DOWN
            private set

        @Volatile
        var error: String? = null
            private set

        @JvmStatic
        fun getAndroidVersion(): String = Build.VERSION.RELEASE ?: "unknown"

        @JvmStatic
        fun getDeviceString(): String =
            listOf(Build.MANUFACTURER, Build.MODEL)
                .filter { !it.isNullOrBlank() }
                .joinToString(" ")

        fun start(
            context: Context,
            name: String,
            accent: String,
            config: String,
            user: String,
            pass: String,
        ) {
            state = TunnelState.CONNECTING
            error = null
            val intent = Intent(context, CharonVpnService::class.java)
                .setAction(ACTION_CONNECT)
                .putExtra(EXTRA_NAME, name)
                .putExtra(EXTRA_ACCENT, accent)
                .putExtra(EXTRA_CONFIG, config)
                .putExtra(EXTRA_USER, user)
                .putExtra(EXTRA_PASS, pass)
            // TunnelService already keeps the process in the foreground (and
            // owns the notification), so this service does not need its own.
            context.startService(intent)
        }

        fun stop(context: Context) {
            instance?.let { service ->
                service.shutdown()
                service.publish(TunnelState.DOWN)
                service.stopSelf()
            }
            runCatching {
                context.stopService(Intent(context, CharonVpnService::class.java))
            }
            if (instance == null) {
                state = TunnelState.DOWN
                error = null
            }
        }
    }

    private var profileName = "IPSec"
    private var initialized = false
    private var connecting = false
    private var trusted: Array<ByteArray> = emptyArray()
    private var userCertificate: Array<ByteArray> = emptyArray()
    private var userKey: PrivateKey? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                shutdown()
                publish(TunnelState.DOWN)
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_CONNECT -> {
                profileName = intent.getStringExtra(EXTRA_NAME).orEmpty().ifBlank { "IPSec" }
                val config = intent.getStringExtra(EXTRA_CONFIG).orEmpty()
                val user = intent.getStringExtra(EXTRA_USER).orEmpty()
                val pass = intent.getStringExtra(EXTRA_PASS).orEmpty()
                if (!connecting) {
                    connecting = true
                    Thread { connect(config, user, pass) }.start()
                }
                return START_NOT_STICKY
            }
        }
        return START_NOT_STICKY
    }

    override fun onRevoke() {
        Log.i(TAG, "VPN revoked by the system")
        shutdown()
        publish(TunnelState.DOWN)
        stopSelf()
        super.onRevoke()
    }

    override fun onDestroy() {
        shutdown()
        if (state != TunnelState.ERROR) {
            publish(TunnelState.DOWN)
        }
        if (instance === this) {
            instance = null
        }
        super.onDestroy()
    }

    // --------------------------------------------------------------- connect

    private fun connect(xml: String, fallbackUser: String, fallbackPass: String) {
        val config = IpSecConfig.parse(xml)
        if (config.server.isBlank()) {
            fail("profile has no gateway address")
            return
        }
        loadCredentials(config)

        val appDir = File(filesDir, "ipsec").apply { mkdirs() }
        val builder = Builder()
            .setSession(profileName)
            .setMtu(config.mtu)
            .setConfigureIntent(configureIntent())
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            builder.setMetered(false)
        }

        val started = initializeCharon(
            BuilderAdapter(builder) { Builder() },
            File(appDir, "charon.log").absolutePath,
            appDir.absolutePath,
            false,
            config.type.startsWith("ikev2-byod"),
        )
        if (!started) {
            fail("charon failed to start")
            return
        }
        initialized = true

        val user = config.username ?: fallbackUser.takeIf { it.isNotBlank() }
        val pass = config.password ?: fallbackPass.takeIf { it.isNotBlank() }
        Log.i(TAG, "connecting to ${config.server}:${config.port} as ${config.type}")
        initiate(settings(config, user, pass))
    }

    private fun configureIntent(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** Serializes the connection settings in libstrongswan's settings format. */
    private fun settings(config: IpSecConfig, user: String?, pass: String?): String = buildString {
        appendLine("global {")
        appendLine("    mtu=\"${config.mtu}\"")
        appendLine("    nat_keepalive=\"20\"")
        appendLine("    rsa_pss=\"1\"")
        appendLine("    crl=\"1\"")
        appendLine("    ocsp=\"1\"")
        appendLine("}")
        appendLine("connection {")
        appendLine("    type=\"${quote(config.type)}\"")
        appendLine("    server=\"${quote(config.server)}\"")
        appendLine("    port=\"${config.port}\"")
        user?.let { appendLine("    username=\"${quote(it)}\"") }
        pass?.let { appendLine("    password=\"${quote(it)}\"") }
        config.localId?.let { appendLine("    local_id=\"${quote(it)}\"") }
        config.remoteId?.let { appendLine("    remote_id=\"${quote(it)}\"") }
        appendLine("    certreq=\"1\"")
        appendLine("    strict_revocation=\"0\"")
        config.ikeProposal?.let { appendLine("    ike_proposal=\"${quote(it)}\"") }
        config.espProposal?.let { appendLine("    esp_proposal=\"${quote(it)}\"") }
        appendLine("}")
    }

    private fun quote(value: String): String =
        value.replace("\\", "\\\\").replace("\"", "\\\"")

    private fun loadCredentials(config: IpSecConfig) {
        trusted = parseCertificates(config.ca)
        userCertificate = parseCertificates(config.cert)
        userKey = config.key?.let { parsePrivateKey(it) }
        Log.i(
            TAG,
            "credentials: ${trusted.size} CA, ${userCertificate.size} client cert, key=${userKey != null}",
        )
    }

    private fun parseCertificates(pem: String?): Array<ByteArray> {
        if (pem.isNullOrBlank()) {
            return emptyArray()
        }
        val text = if (pem.contains("BEGIN CERTIFICATE")) {
            pem
        } else {
            "-----BEGIN CERTIFICATE-----\n${pem.trim()}\n-----END CERTIFICATE-----\n"
        }
        return runCatching {
            val factory = CertificateFactory.getInstance("X.509")
            factory
                .generateCertificates(ByteArrayInputStream(text.toByteArray()))
                .map { it.encoded }
                .toTypedArray()
        }.getOrElse {
            Log.w(TAG, "could not parse certificates: ${it.message}")
            emptyArray()
        }
    }

    private fun parsePrivateKey(pem: String): PrivateKey? {
        val body = pem
            .replace(Regex("-----BEGIN [A-Z ]+-----"), "")
            .replace(Regex("-----END [A-Z ]+-----"), "")
            .filterNot { it.isWhitespace() }
        val der = runCatching { Base64.decode(body, Base64.DEFAULT) }.getOrNull() ?: return null
        val spec = PKCS8EncodedKeySpec(der)
        for (algorithm in listOf("RSA", "EC", "Ed25519")) {
            runCatching { KeyFactory.getInstance(algorithm).generatePrivate(spec) }
                .getOrNull()
                ?.let { return it }
        }
        Log.w(TAG, "could not parse the client private key")
        return null
    }

    // -------------------------------------------------------------- callbacks

    /** Called by charon on every state change (see charonservice.h). */
    fun updateStatus(code: Int) {
        Log.i(TAG, "state: $code")
        when (code) {
            CHILD_SA_UP -> publish(TunnelState.UP)
            CHILD_SA_DOWN -> publish(TunnelState.CONNECTING)
            AUTH_ERROR -> fail("authentication failed")
            PEER_AUTH_ERROR -> fail("server authentication failed")
            LOOKUP_ERROR -> fail("gateway could not be resolved")
            UNREACHABLE_ERROR -> fail("gateway unreachable")
            CERTIFICATE_UNAVAILABLE -> fail("client certificate unavailable")
            GENERIC_ERROR -> fail("IPSec negotiation failed")
            else -> Log.w(TAG, "unknown state code $code")
        }
    }

    fun updateImcState(state: Int) {}

    fun addRemediationInstruction(instruction: String) {
        Log.i(TAG, "remediation: $instruction")
    }

    fun getTrustedCertificates(): Array<ByteArray> = trusted

    fun getUserCertificate(): Array<ByteArray> = userCertificate

    fun getUserKey(): PrivateKey? = userKey

    // ---------------------------------------------------------------- helpers

    private fun publish(newState: TunnelState, message: String? = null) {
        state = newState
        error = message
        Log.i(TAG, "state -> ${newState.name}" + (message?.let { " ($it)" } ?: ""))
    }

    private fun fail(message: String) {
        Log.w(TAG, "IPSec failed: $message")
        publish(TunnelState.ERROR, message)
        shutdown()
        stopSelf()
    }

    private fun shutdown() {
        if (initialized) {
            initialized = false
            Log.i(TAG, "deinit: start")
            // charon's teardown can block while it winds down its threads.
            // Never let that hold up the caller: the VpnService is stopped
            // right after this, which is what actually removes the tun.
            Thread {
                runCatching { deinitializeCharon() }
                Log.i(TAG, "deinit: done")
            }.apply { isDaemon = true }.start()
        }
    }

    // ---------------------------------------------------------------- natives

    external fun initializeCharon(
        builder: BuilderAdapter,
        logfile: String,
        appdir: String,
        byod: Boolean,
        ipv6: Boolean,
    ): Boolean

    external fun deinitializeCharon()

    external fun initiate(config: String)

    /**
     * Wraps [VpnService.Builder] for the native code. strongSwan adds addresses,
     * routes, DNS servers and the MTU, then establishes the tun device here.
     */
    class BuilderAdapter(
        private var builder: VpnService.Builder,
        private val newBuilder: () -> VpnService.Builder,
    ) {
        private var established: VpnService.Builder? = null

        @Synchronized
        fun addAddress(address: String, prefixLength: Int): Boolean = runCatching {
            builder.addAddress(address, prefixLength)
            true
        }.getOrDefault(false)

        @Synchronized
        fun addRoute(address: String, prefixLength: Int): Boolean = runCatching {
            builder.addRoute(address, prefixLength)
            true
        }.getOrDefault(false)

        @Synchronized
        fun addDnsServer(address: String): Boolean = runCatching {
            builder.addDnsServer(address)
            true
        }.getOrDefault(false)

        @Synchronized
        fun setMtu(mtu: Int): Boolean = runCatching {
            builder.setMtu(mtu)
            true
        }.getOrDefault(false)

        @Synchronized
        fun establish(): Int {
            val fd: ParcelFileDescriptor = runCatching { builder.establish() }.getOrNull()
                ?: return -1
            established = builder
            builder = newBuilder()
            return fd.detachFd()
        }

        @Synchronized
        fun establishNoDns(): Int {
            val previous = established ?: return -1
            val fd: ParcelFileDescriptor = runCatching { previous.establish() }.getOrNull()
                ?: return -1
            return fd.detachFd()
        }
    }
}
