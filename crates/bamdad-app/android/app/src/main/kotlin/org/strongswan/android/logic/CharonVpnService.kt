package org.strongswan.android.logic

import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import java.io.File
import java.security.PrivateKey

/**
 * Host for the vendored strongSwan charon daemon.
 *
 * The native bridge (`libandroidbridge.so`) resolves this exact class name and
 * the `BuilderAdapter` nested class by name, so it lives in strongSwan's
 * package. Only the surface the native code calls is implemented here; the
 * upstream UI, profile database, and notification code are not needed.
 *
 * This class is not a finished backend yet: it boots charon, which is the
 * milestone that proves the packaged libraries load and initialize on a
 * device. The IKEv2 connect path (initiate, state mapping, profile import)
 * extends this class next.
 */
class CharonVpnService : VpnService() {
    companion object {
        private const val TAG = "BamdadCharon"

        init {
            System.loadLibrary("androidbridge")
        }

        @JvmStatic
        fun getAndroidVersion(): String = Build.VERSION.RELEASE ?: "unknown"

        @JvmStatic
        fun getDeviceString(): String =
            listOf(Build.MANUFACTURER, Build.MODEL)
                .filter { !it.isNullOrBlank() }
                .joinToString(" ")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Thread {
            runCatching { selfTest() }
                .onFailure { Log.e(TAG, "self-test failed", it) }
            Log.i(TAG, "ipsec self-test complete")
            stopSelf()
        }.start()
        return START_NOT_STICKY
    }

    /** Boot charon and tear it down again: proves the native stack works. */
    private fun selfTest() {
        val appDir = File(filesDir, "ipsec").apply { mkdirs() }
        val builder = Builder()
            .setSession("Bamdad IPSec self-test")
            .setMtu(1400)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            builder.setMetered(false)
        }
        builder.setConfigureIntent(
            PendingIntent.getActivity(
                this,
                0,
                Intent(this, com.mehrnet.bamdad.MainActivity::class.java),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            ),
        )

        val started = initializeCharon(
            BuilderAdapter(builder) { Builder() },
            File(appDir, "charon.log").absolutePath,
            appDir.absolutePath,
            false,
            false,
        )
        Log.i(TAG, "charon init: $started")
        if (started) {
            deinitializeCharon()
            Log.i(TAG, "charon deinit: ok")
        }
    }

    // ---------------------------------------------------------------- native

    external fun initializeCharon(
        builder: BuilderAdapter,
        logfile: String,
        appdir: String,
        byod: Boolean,
        ipv6: Boolean,
    ): Boolean

    external fun deinitializeCharon()

    external fun initiate(config: String)

    // ------------------------------------------------------------- callbacks

    /** Called by charon on every state change; codes match the native enum. */
    fun updateStatus(code: Int) {
        Log.i(TAG, "state: $code")
    }

    fun updateImcState(state: Int) {}

    fun addRemediationInstruction(instruction: String) {
        Log.i(TAG, "remediation: $instruction")
    }

    fun getTrustedCertificates(): Array<ByteArray> = emptyArray()

    fun getUserCertificate(): Array<ByteArray> = emptyArray()

    fun getUserKey(): PrivateKey? = null

    /**
     * Wraps [VpnService.Builder] for the native code. strongSwan adds addresses,
     * routes, DNS servers and the MTU, then establishes the tun device from here.
     */
    class BuilderAdapter(
        private var builder: VpnService.Builder,
        private val newBuilder: () -> VpnService.Builder,
    ) {
        private var established: Builder? = null

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
            // Keep the configured builder so a re-establish can reuse it.
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
